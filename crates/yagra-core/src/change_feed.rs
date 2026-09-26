// SPDX-License-Identifier: AGPL-3.0-only
//! The **change feed** browsers follow: a revision number that moves whenever something an open
//! screen shows may have changed (ADR-019 増分 2).
//!
//! `GET /api/v1/stream/config` sends it, and the WebUI re-reads the inventory tree and the other
//! configuration screens when it differs from the last one it saw. Only the number travels — not
//! what changed — which is why the stream is refused to a group-scoped caller (ADR-014: a scoped
//! account is not told that something happened outside its folders).
//!
//! ⚠️ **Not [`crate::config_gen`], though every bump of that one moves this one too.** The config
//! generation wakes the scheduler's sweep through a `Notify` with **one** waiter; a browser waiting
//! on it would take turns with the sweep for the same permit. This is a `broadcast`, so any number
//! of open tabs can follow it without touching the sweep.
//!
//! It also moves on changes that are *not* config generations: reordering a folder changes what the
//! tree draws and nothing a poller reads, so `audit_mw` publishes it here alone.
//!
//! ⚠️ Process-local, like the config generation. Under HA a browser attached to a standby hears
//! nothing — the load balancer sends it to the leader (`/readyz`), and after a failover the new
//! leader's number differs from the old one's, so the first frame after reconnecting reads as a
//! change and the screen catches up once.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use tokio::sync::broadcast;

static REVISION: AtomicU64 = AtomicU64::new(0);

/// Small on purpose: a subscriber that falls behind has lost nothing but intermediate numbers, and
/// the stream answers a lag by sending the current one.
const CAPACITY: usize = 16;

static SENDER: OnceLock<broadcast::Sender<u64>> = OnceLock::new();

fn sender() -> &'static broadcast::Sender<u64> {
    SENDER.get_or_init(|| broadcast::channel(CAPACITY).0)
}

/// Move the revision and tell every open stream. Cheap enough to call on every write: an atomic add
/// and a broadcast send that is a no-op when nobody is listening.
pub fn publish() {
    let rev = REVISION.fetch_add(1, Ordering::Relaxed) + 1;
    // `Err` only means there is no receiver right now, which is the normal state with no tab open.
    let _ = sender().send(rev);
}

/// The revision as of now.
#[must_use]
pub fn current() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

/// A receiver for the revisions published from now on. Subscribe **before** reading [`current`], or
/// a publish landing between the two is neither in the snapshot nor on the receiver.
#[must_use]
pub fn subscribe() -> broadcast::Receiver<u64> {
    sender().subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_publish_moves_the_revision_and_reaches_every_subscriber() {
        // Process-global and other tests publish in parallel, so assert "moved" and "arrived",
        // never an exact value.
        let (mut a, mut b) = (subscribe(), subscribe());
        let before = current();
        publish();
        assert!(current() > before);
        for rx in [&mut a, &mut b] {
            let got = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("a publish reaches the subscriber")
                .expect("the sender lives for the process");
            assert!(got > before);
        }
    }
}
