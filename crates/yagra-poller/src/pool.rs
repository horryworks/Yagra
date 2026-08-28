// SPDX-License-Identifier: AGPL-3.0-only
//! Which pool this poller serves, and following core when that changes (ADR-107 Inc.2).
//!
//! Before this module the answer was `YAGRA_POLLER_POOL`, read once at startup and never revisited,
//! so moving a poller between pools meant editing a file at the site and recreating the container.
//! Now core owns `pollers.pool`, tells the poller on every working-set snapshot
//! ([`yagra_bus::WorkingSetSnapshot::pool`]), and this module is what makes the running process
//! act on it.
//!
//! ## Why anything has to change at all
//!
//! 🔑 **The working set arrives on `yagra.poller.assign.{id}`, which carries no pool token.** So the
//! *primary* work path — the nodes this poller polls on a schedule — follows a move with no
//! cooperation from this process whatsoever: core rebuilds the ring and the next snapshot is simply
//! a different set of nodes.
//!
//! What does not follow are the **three** subjects derived from the pool name:
//!
//! | subject | carries |
//! |---|---|
//! | `yagra.jobs.{pool}` | "poll now", and legacy per-job dispatch |
//! | `yagra.discovery.jobs.{pool}` | discovery sweeps |
//! | `yagra.discovery.cancel.{pool}` | stopping one |
//!
//! 🚨 **Leaving those pointed at the old pool is the worst available failure**, which is why
//! [`yagra_bus::CAP_POOL_FOLLOW`] exists and why core refuses to move a poller that does not claim
//! it: every screen would read correct — the working set is right, the node counts are right — while
//! "poll now" produced nothing at all, because plain NATS discards a message nobody is subscribed
//! to. No error, anywhere.
//!
//! ## Why a relay rather than re-subscribing in place
//!
//! Those three subscriptions are consumed by three long-lived loops that were written to own a
//! stream for the process lifetime (`worker::run_stream` merges one of them with the locally
//! scheduled jobs). Handing them a stream that has to be *replaced* would mean teaching each of
//! them about pools. Instead [`relay`] puts a bounded channel in front: the consumer holds a
//! receiver that never changes, and this module swaps the subscription behind it.
//!
//! ## Why the reconnect
//!
//! With Auth Callout on (every deployment that accepts remote pollers), the broker grants this
//! connection a JWT minted **once, at connect**, scoped to one pool. Re-subscribing to another
//! pool's subject on that connection is denied. [`Client::force_reconnect`] is upstream's answer —
//! its own documentation names re-triggering the auth callback as the use case — and core now mints
//! the scope from the inventory row rather than from the `CONNECT` name, so the new connection
//! comes back with the right grant. **Nothing restarts**: the process, the working set, the
//! in-flight probes and every other subscription survive; async-nats re-establishes them itself.
//!
//! Done unconditionally rather than only under callout, because this process cannot tell which
//! configuration it is on — and on the static-account deployment the reconnect is a few tens of
//! milliseconds with no behavioural effect at all.

use std::sync::Arc;
use std::time::Duration;

use futures::Stream;
use tokio::sync::{mpsc, watch};
use yagra_bus::NatsBus;
use yagra_telemetry::{spawn_cancellable, CancellationToken};

/// How many messages may wait in a relay's channel.
///
/// Small on purpose: these are operator-initiated ("poll now", a sweep, a stop), not a fleet-rate
/// stream, and a deep buffer would only delay the backpressure that already exists downstream.
const RELAY_DEPTH: usize = 64;

/// How long a relay waits before re-subscribing after the bus refused or ended a subscription.
///
/// A subscription ending is normal (it happens on every reconnect, including the one this module
/// asks for), so the loop must retry — but retrying instantly against a broker that is down is a
/// spin. Interrupted by a pool change, so a move is never delayed by it.
const RESUBSCRIBE_BACKOFF: Duration = Duration::from_secs(1);

/// The pool this poller currently serves, and the signal that it changed.
///
/// A [`watch`] channel rather than a mutex plus a notify: the two things every reader wants are
/// "what is it now" and "tell me when it is different", and a watch is exactly those two with no
/// possibility of them disagreeing.
pub(crate) struct PoolState {
    tx: watch::Sender<String>,
    /// The bus to reconnect when the pool changes. `None` in tests, where there is no broker and
    /// the re-subscription is the only half worth exercising.
    bus: Option<Arc<NatsBus>>,
}

impl PoolState {
    /// Start from what this poller's own environment says — the value core will also use if it has
    /// never seen this poller before.
    pub(crate) fn new(initial: String, bus: Option<Arc<NatsBus>>) -> Arc<Self> {
        let (tx, _rx) = watch::channel(initial);
        Arc::new(Self { tx, bus })
    }

    /// The pool to report, to label with, and to subscribe to right now.
    pub(crate) fn current(&self) -> String {
        self.tx.borrow().clone()
    }

    /// A receiver that fires when the pool changes.
    pub(crate) fn watch(&self) -> watch::Receiver<String> {
        self.tx.subscribe()
    }

    /// Take core's answer.
    ///
    /// Reconnects **before** publishing the change, so the relays' new subscriptions are created
    /// against a connection that is already on its way to holding the new grant rather than against
    /// the old one — which would log a permissions violation, be re-established on the reconnect
    /// anyway, and leave an alarming line in the log for a move that worked.
    ///
    /// A no-op when the pool is unchanged, which is the case on all but one snapshot in the life of
    /// a poller: core sends the name on every snapshot, and a resync is common.
    pub(crate) async fn adopt(&self, pool: &str) -> bool {
        if *self.tx.borrow() == pool {
            return false;
        }
        let from = self.current();
        tracing::info!(
            from = %from,
            to = %pool,
            "core moved this poller to another pool — re-pointing the bus without restarting"
        );
        if let Some(bus) = &self.bus {
            // A failure here is not fatal and must not stop the change: on a deployment without
            // Auth Callout there is nothing to re-mint, and even with it the client reconnects on
            // its own eventually. Re-subscribing to a subject this connection may not read yet is
            // recoverable; not re-subscribing at all is not.
            if let Err(e) = bus.force_reconnect().await {
                tracing::warn!(error = %e, "could not force a bus reconnect for the pool change");
            }
        }
        self.tx.send_replace(pool.to_owned());
        metrics::counter!("yagra_poller_pool_changes_total").increment(1);
        true
    }
}

/// Forward one pool-derived subscription into a channel whose receiving half never changes,
/// re-subscribing whenever [`PoolState`] says the pool moved.
///
/// `subscribe` is called with the pool name and must produce the stream for it. Taking a closure
/// rather than an enum keeps this module ignorant of the three message types — the alternative was
/// three near-identical loops differing only in a bus method and a noun (extensibility.md §3).
pub(crate) fn relay<T, F, Fut, S>(
    what: &'static str,
    pool: &Arc<PoolState>,
    shutdown: &CancellationToken,
    subscribe: F,
) -> mpsc::Receiver<T>
where
    T: Send + 'static,
    S: Stream<Item = T> + Send,
    Fut: std::future::Future<Output = Result<S, yagra_bus::BusError>> + Send,
    F: Fn(String) -> Fut + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<T>(RELAY_DEPTH);
    let mut changed = pool.watch();
    spawn_cancellable(shutdown, async move {
        use futures::StreamExt;
        let mut current = changed.borrow_and_update().clone();
        loop {
            let stream = match subscribe(current.clone()).await {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!(error = %e, what, pool = %current, "subscribe failed — retrying");
                    None
                }
            };
            let Some(stream) = stream else {
                // Wait out the backoff, but let a pool change cut it short.
                tokio::select! {
                    _ = tokio::time::sleep(RESUBSCRIBE_BACKOFF) => {}
                    Ok(()) = changed.changed() => current = changed.borrow_and_update().clone(),
                }
                continue;
            };
            // Boxed here rather than demanded of the caller: the bus returns a `filter_map`
            // combinator, which is not `Unpin`, and requiring it would push a `Box::pin` into all
            // three call sites for a detail none of them cares about.
            let mut stream = Box::pin(stream);
            tracing::debug!(what, pool = %current, "subscribed");
            loop {
                tokio::select! {
                    // Biased so a pool change is taken the moment it is available, rather than
                    // after however many messages the old pool still has queued — those are jobs
                    // for a pool this poller no longer serves.
                    biased;
                    res = changed.changed() => {
                        if res.is_err() {
                            return; // the state was dropped: the process is going away
                        }
                        current = changed.borrow_and_update().clone();
                        break; // dropping `stream` unsubscribes
                    }
                    item = stream.next() => match item {
                        Some(v) => {
                            if tx.send(v).await.is_err() {
                                return; // consumer gone
                            }
                        }
                        None => {
                            // The subscription ended — a reconnect, or the broker going away.
                            // Re-subscribe rather than going quiet, which is what would otherwise
                            // happen to "poll now" after every bus blip.
                            tokio::time::sleep(RESUBSCRIBE_BACKOFF).await;
                            break;
                        }
                    },
                }
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two halves of a move, in the order they must happen: the value changes, and everything
    /// watching is told. A relay that never learned would keep the old pool's subscriptions —
    /// which is the exact silent failure `CAP_POOL_FOLLOW` exists to keep core from causing.
    #[tokio::test]
    async fn adopting_a_new_pool_publishes_it_to_every_watcher() {
        let state = PoolState::new("default".to_owned(), None);
        let mut w = state.watch();
        assert_eq!(state.current(), "default");

        assert!(state.adopt("tokyo").await, "a real change is reported");
        assert_eq!(state.current(), "tokyo");
        assert!(w.changed().await.is_ok(), "the watcher was woken");
        assert_eq!(*w.borrow_and_update(), "tokyo");
    }

    /// Core sends the pool on **every** snapshot, and a poller resyncs for many reasons that are
    /// not moves. Treating each of those as a change would reconnect the bus — dropping every
    /// subscription in the process — on a routine gap recovery.
    #[tokio::test]
    async fn re_adopting_the_same_pool_is_not_a_change() {
        let state = PoolState::new("tokyo".to_owned(), None);
        let mut w = state.watch();
        assert!(!state.adopt("tokyo").await, "no move, no change");
        // `changed()` would resolve immediately if a value had been published.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), w.changed())
                .await
                .is_err(),
            "nothing may be published when the pool did not move"
        );
    }

    /// The relay's contract: the consumer's receiver survives a pool change, and what arrives after
    /// one came from the **new** pool's subscription. Without this the move would look like it
    /// worked and "poll now" would silently keep going to the old pool.
    #[tokio::test]
    async fn the_relay_reopens_on_the_new_pool_and_keeps_one_receiver() {
        use std::sync::Mutex;

        let state = PoolState::new("default".to_owned(), None);
        let shutdown = CancellationToken::new();
        // Each subscription yields exactly one item naming the pool it was opened for, then ends.
        let opened: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen = opened.clone();
        let mut rx = relay("test", &state, &shutdown, move |pool: String| {
            let seen = seen.clone();
            async move {
                seen.lock().expect("poisoned").push(pool.clone());
                Ok::<_, yagra_bus::BusError>(Box::pin(futures::stream::iter(vec![pool])))
            }
        });

        assert_eq!(rx.recv().await.as_deref(), Some("default"));
        state.adopt("tokyo").await;
        // The same receiver, now fed by the new pool's subscription.
        let next = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the relay re-subscribed within the backoff");
        assert_eq!(next.as_deref(), Some("tokyo"));
        assert!(
            opened
                .lock()
                .expect("poisoned")
                .contains(&"tokyo".to_owned()),
            "the new pool was actually subscribed"
        );
        shutdown.cancel();
    }
}
