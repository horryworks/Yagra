// SPDX-License-Identifier: AGPL-3.0-only
//! Poll rate control / backpressure (Workstream #4).
//!
//! Two guards protect both Yagra and the devices (monitoring-conventions):
//! - **Per-device single-flight**: never *run* two probes against one target at once. A second job
//!   waits for the first to finish, up to a deadline, and is dropped (counted) only if the device
//!   is still busy then — so a slow device still cannot pile polls up unbounded.
//! - **Global concurrency cap**: a semaphore bounds total in-flight probes, so the worker
//!   applies backpressure instead of spawning unboundedly.
//!
//! ⚠️ **The second job used to be dropped immediately, and that starved every spec after the
//! first.** A node's specs are staggered ~1s apart (`working_set::SPEC_STAGGER_MS`), but a real
//! table walk takes far longer: measured 1.2s–6.0s against devices with 19–232 interfaces. So the
//! later spec always arrived while the earlier one was genuinely still walking, lost, and was
//! discarded — and because `working_set::due` advances `next_due` when it *emits* a job rather than
//! when the job succeeds, it lost again next cycle, and every cycle after. Measured on the test
//! deployment before this change: 819 dropped against 1376 executed (**37%**), with the optical
//! spec — last in the list — reporting 0 samples in an hour while the table spec ahead of it
//! reported 8 of 8. **The stagger was never the guarantee; it is a hint. Serialising is the
//! guarantee**, and one device's specs genuinely have to be serialised because they are one
//! conversation with one agent.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

/// How often a waiter re-checks the in-flight set even if it sees no release notification.
///
/// Not belt-and-braces: [`Notify::notify_waiters`] only wakes tasks already parked, so a release
/// landing between a waiter's check and its await is missed. Re-checking on a short tick turns that
/// race into at most one extra tick of latency instead of a poll that waits out its whole deadline.
const RECHECK_INTERVAL: Duration = Duration::from_millis(50);

/// Held for the duration of one probe; on drop it frees the global permit and clears the
/// target's single-flight marker.
pub struct PollGuard {
    target: IpAddr,
    inflight: Arc<Mutex<HashSet<IpAddr>>>,
    released: Arc<Notify>,
    _permit: OwnedSemaphorePermit,
}

impl Drop for PollGuard {
    fn drop(&mut self) {
        // Never panic in `drop`: a panic during unwinding aborts the whole process. Recover
        // from a poisoned lock so the single-flight marker is still cleared (a leaked marker
        // would block that target from ever being polled again).
        self.inflight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.target);
        // Wake anything waiting on this device. Cheap and unconditional: the waiters re-check the
        // set anyway, so waking one that wanted a different target costs a lock and a loop.
        self.released.notify_waiters();
    }
}

/// Per-device single-flight + global concurrency limiter.
pub struct PollLimiter {
    global: Arc<Semaphore>,
    inflight: Arc<Mutex<HashSet<IpAddr>>>,
    /// Notified whenever a guard drops, so a waiter re-checks without spinning.
    released: Arc<Notify>,
}

impl PollLimiter {
    /// New limiter allowing `max_concurrent` simultaneous probes (clamped to >= 1).
    #[must_use]
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(max_concurrent.max(1))),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            released: Arc::new(Notify::new()),
        }
    }

    /// Begin a probe for `target`, waiting up to `max_wait` for one already running against it.
    ///
    /// Returns `None` when the device is *still* busy at the deadline — the caller counts that as a
    /// skipped poll, exactly as before — or on shutdown. `max_wait` is what keeps this
    /// backpressure rather than an unbounded queue: a device that never frees up sheds load.
    ///
    /// **The global permit is taken first, and the target is marked only once the probe is about to
    /// run.** The previous order marked the target and *then* queued for a permit, so time spent
    /// waiting for concurrency counted as "this device is being polled" even though no packet had
    /// been sent.
    pub async fn begin_for(&self, target: IpAddr, max_wait: Duration) -> Option<PollGuard> {
        let permit = self.global.clone().acquire_owned().await.ok()?;
        let deadline = Instant::now() + max_wait;
        loop {
            {
                let mut set = self.inflight.lock().expect("inflight mutex poisoned");
                if set.insert(target) {
                    return Some(PollGuard {
                        target,
                        inflight: self.inflight.clone(),
                        released: self.released.clone(),
                        _permit: permit,
                    });
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            // Woken by a release or by the tick, whichever comes first (see `RECHECK_INTERVAL`).
            tokio::select! {
                () = self.released.notified() => {}
                () = tokio::time::sleep(RECHECK_INTERVAL) => {}
            }
        }
    }

    /// Acquire only the global concurrency permit — **no** per-target single-flight. For jobs whose
    /// single-flight is enforced elsewhere and which share a sentinel target (Meraki org collectors
    /// all carry `0.0.0.0`, gated per-org by core), so per-device single-flight would wrongly drop
    /// concurrent collects for different orgs. Awaits a permit (backpressure); returns `None` only
    /// on shutdown. The returned guard's sentinel target is never inserted into the inflight set, so
    /// its drop is a harmless no-op there.
    pub async fn begin_global(&self) -> Option<PollGuard> {
        self.global
            .clone()
            .acquire_owned()
            .await
            .ok()
            .map(|permit| PollGuard {
                target: IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
                inflight: self.inflight.clone(),
                released: self.released.clone(),
                _permit: permit,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn target(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }

    /// Zero wait is the old behaviour, and it is still reachable: a busy device refuses at once.
    #[tokio::test]
    async fn single_flight_blocks_second_probe_for_same_target() {
        let limiter = PollLimiter::new(8);
        let g1 = limiter.begin_for(target(1), Duration::ZERO).await;
        assert!(g1.is_some());
        // Same target while the first is in flight → refused without waiting.
        assert!(limiter.begin_for(target(1), Duration::ZERO).await.is_none());
        // A different target is fine.
        assert!(limiter.begin_for(target(2), Duration::ZERO).await.is_some());
        // Once the first guard drops, the target can be probed again.
        drop(g1);
        assert!(limiter.begin_for(target(1), Duration::ZERO).await.is_some());
    }

    /// **The starvation fix, stated as a property.** A node's specs land ~1s apart while a walk
    /// takes seconds, so the second one must *wait for its turn* rather than be thrown away. With
    /// the old drop-immediately behaviour this returns `None` and the test fails.
    #[tokio::test]
    async fn a_second_spec_for_one_device_waits_its_turn_instead_of_being_dropped() {
        let limiter = Arc::new(PollLimiter::new(8));
        let first = limiter
            .begin_for(target(1), Duration::ZERO)
            .await
            .expect("first spec starts");

        // The walk finishes well inside the waiter's deadline.
        let holder = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            drop(first);
        });

        let started = Instant::now();
        let second = limiter.begin_for(target(1), Duration::from_secs(5)).await;
        assert!(
            second.is_some(),
            "the later spec must run once the device frees up, not be discarded"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "it must actually have waited for the first probe, not raced past it"
        );
        holder.await.expect("holder task");
    }

    /// The wait is bounded, so a device that never frees up still sheds load — that is what keeps
    /// this backpressure. Without a deadline the queue would grow without limit.
    #[tokio::test]
    async fn a_device_that_never_frees_up_still_sheds_the_poll() {
        let limiter = PollLimiter::new(8);
        let _held = limiter
            .begin_for(target(1), Duration::ZERO)
            .await
            .expect("first spec starts");
        let started = Instant::now();
        assert!(
            limiter
                .begin_for(target(1), Duration::from_millis(150))
                .await
                .is_none(),
            "still busy at the deadline → skip, exactly as before"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(150),
            "it waited out the deadline rather than refusing early"
        );
    }

    #[tokio::test]
    async fn drop_recovers_from_poisoned_lock_and_clears_marker() {
        let limiter = PollLimiter::new(8);
        let guard = limiter.begin_for(target(1), Duration::ZERO).await.unwrap();

        // Poison the inflight mutex: a separate thread panics while holding the lock.
        // (Expect a "panicked at ... poison" line on stderr — it is intentional.)
        let inflight = limiter.inflight.clone();
        let _ = std::thread::spawn(move || {
            let _held = inflight.lock().unwrap();
            panic!("intentionally poison the lock");
        })
        .join();

        // Dropping the guard must NOT panic even though the lock is poisoned (a panic in
        // `drop` during unwinding would abort the process), and it must still clear the
        // single-flight marker.
        drop(guard);

        let set = limiter
            .inflight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            !set.contains(&target(1)),
            "marker should be cleared even after lock poisoning"
        );
    }

    #[tokio::test]
    async fn global_cap_limits_concurrency() {
        let limiter = PollLimiter::new(1);
        let _g = limiter.begin_for(target(1), Duration::ZERO).await.unwrap();
        // The single global permit is taken; a different target cannot start yet.
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                limiter.begin_for(target(2), Duration::ZERO),
            )
            .await
            .is_err(),
            "should block on the global permit"
        );
    }
}
