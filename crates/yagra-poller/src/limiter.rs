// SPDX-License-Identifier: AGPL-3.0-only
//! Poll rate control / backpressure (Workstream #4).
//!
//! Two guards protect both Yagra and the devices (monitoring-conventions):
//! - **Per-device single-flight**: never start a new probe for a target while its previous
//!   probe is still running. A slow/struggling device can't pile polls up unbounded — the
//!   extra job is dropped (counted), not queued.
//! - **Global concurrency cap**: a semaphore bounds total in-flight probes, so the worker
//!   applies backpressure instead of spawning unboundedly.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Held for the duration of one probe; on drop it frees the global permit and clears the
/// target's single-flight marker.
pub struct PollGuard {
    target: IpAddr,
    inflight: Arc<Mutex<HashSet<IpAddr>>>,
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
    }
}

/// Per-device single-flight + global concurrency limiter.
pub struct PollLimiter {
    global: Arc<Semaphore>,
    inflight: Arc<Mutex<HashSet<IpAddr>>>,
}

impl PollLimiter {
    /// New limiter allowing `max_concurrent` simultaneous probes (clamped to >= 1).
    #[must_use]
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(max_concurrent.max(1))),
            inflight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Try to begin a probe for `target`. Returns a [`PollGuard`] to hold for the probe's
    /// duration, or `None` if a probe for that target is already in flight (skip it). Awaits
    /// a global permit first (backpressure under load).
    pub async fn try_begin(&self, target: IpAddr) -> Option<PollGuard> {
        {
            let mut set = self.inflight.lock().expect("inflight mutex poisoned");
            if !set.insert(target) {
                return None; // already in flight → skip this poll
            }
        }
        match self.global.clone().acquire_owned().await {
            Ok(permit) => Some(PollGuard {
                target,
                inflight: self.inflight.clone(),
                _permit: permit,
            }),
            Err(_) => {
                // Semaphore closed (shutdown) — unmark and skip.
                self.inflight
                    .lock()
                    .expect("inflight mutex poisoned")
                    .remove(&target);
                None
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

    #[tokio::test]
    async fn single_flight_blocks_second_probe_for_same_target() {
        let limiter = PollLimiter::new(8);
        let g1 = limiter.try_begin(target(1)).await;
        assert!(g1.is_some());
        // Same target while the first is in flight → skipped.
        assert!(limiter.try_begin(target(1)).await.is_none());
        // A different target is fine.
        assert!(limiter.try_begin(target(2)).await.is_some());
        // Once the first guard drops, the target can be probed again.
        drop(g1);
        assert!(limiter.try_begin(target(1)).await.is_some());
    }

    #[tokio::test]
    async fn drop_recovers_from_poisoned_lock_and_clears_marker() {
        let limiter = PollLimiter::new(8);
        let guard = limiter.try_begin(target(1)).await.unwrap();

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
        let _g = limiter.try_begin(target(1)).await.unwrap();
        // The single global permit is taken; a different target cannot start yet.
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                limiter.try_begin(target(2)),
            )
            .await
            .is_err(),
            "should block on the global permit"
        );
    }
}
