// SPDX-License-Identifier: AGPL-3.0-only
//! Notification dispatch: dedup + retry over pluggable channels.
//!
//! Yagra forwards a *clean* alert signal to external tools — escalation/on-call live there
//! (ADR-015). This module owns the quality gate around delivery: it **dedups** so a still-active
//! alert is not re-sent, and **retries with backoff** on transient channel failure. The actual
//! email/Webhook transport is a [`NotifyChannel`] implementation (an I/O adapter); the dispatcher
//! and its policy are pure and tested against a fake channel.

use crate::alert::{Alert, DedupKey};
use async_trait::async_trait;
use std::collections::HashSet;
use std::time::Duration;
use thiserror::Error;
use yagra_common::Severity;

/// A rendered notification ready to hand to a channel. Content is templated upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// Identity for dedup (same key = same alert).
    pub dedup_key: DedupKey,
    /// Severity headline.
    pub severity: Severity,
    /// Short human summary (subject line).
    pub summary: String,
    /// Rendered payload (JSON/text) for the channel.
    pub payload: String,
}

impl Notification {
    /// Build a notification for an alert with a pre-rendered summary/payload.
    #[must_use]
    pub fn for_alert(
        alert: &Alert,
        summary: impl Into<String>,
        payload: impl Into<String>,
    ) -> Self {
        Self {
            dedup_key: alert.dedup_key(),
            severity: alert.severity,
            summary: summary.into(),
            payload: payload.into(),
        }
    }
}

/// A delivery channel (email, Webhook/PagerDuty/JSM, …). Implementations are I/O adapters.
#[async_trait]
pub trait NotifyChannel: Send + Sync {
    /// Attempt one delivery. Returning `Err` triggers the dispatcher's retry policy.
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError>;

    /// Deliver a resolve for a previously-fired notification. Channels with no lifecycle
    /// concept (webhook, email) keep this default no-op; incident-style channels
    /// (PagerDuty, JSM) override it to close the remote incident.
    async fn deliver_resolve(&self, _notification: &Notification) -> Result<(), NotifyError> {
        Ok(())
    }
}

/// Lets a boxed/shared trait object be used directly as the `Dispatcher`'s channel — so a
/// router can keep a `Dispatcher<Arc<dyn NotifyChannel>>` per dynamically-configured channel.
/// Both methods must forward, or a concrete channel's resolve override would be shadowed
/// by the trait default.
#[async_trait]
impl NotifyChannel for std::sync::Arc<dyn NotifyChannel> {
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError> {
        (**self).deliver(notification).await
    }

    async fn deliver_resolve(&self, notification: &Notification) -> Result<(), NotifyError> {
        (**self).deliver_resolve(notification).await
    }
}

/// Errors from a channel.
#[derive(Debug, Error)]
pub enum NotifyError {
    /// Transient or permanent delivery failure (message is channel-specific).
    #[error("delivery failed: {0}")]
    Delivery(String),
}

/// Retry behaviour for a flaky channel.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Total attempts before giving up (>= 1).
    pub max_attempts: u32,
    /// Base backoff; attempt `n` waits `base * 2^(n-1)`.
    pub base_backoff_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_backoff_ms: 500,
        }
    }
}

/// Outcome of dispatching one notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// Delivered (after up to `attempts` tries).
    Delivered { attempts: u32 },
    /// Suppressed as a duplicate of a still-active alert (channel not called).
    Suppressed,
    /// All retries exhausted without success.
    Failed { attempts: u32 },
}

/// Dedups and delivers notifications over a channel, applying the retry policy.
pub struct Dispatcher<C: NotifyChannel> {
    channel: C,
    policy: RetryPolicy,
    active: HashSet<DedupKey>,
}

impl<C: NotifyChannel> Dispatcher<C> {
    /// New dispatcher over a channel with a retry policy.
    pub fn new(channel: C, policy: RetryPolicy) -> Self {
        Self {
            channel,
            policy,
            active: HashSet::new(),
        }
    }

    /// Dispatch a notification: suppress duplicates of active alerts, else deliver with retry.
    pub async fn dispatch(&mut self, notification: Notification) -> DispatchOutcome {
        if self.active.contains(&notification.dedup_key) {
            return DispatchOutcome::Suppressed;
        }
        match self.deliver_with_retry(&notification, false).await {
            Ok(attempts) => {
                self.active.insert(notification.dedup_key);
                DispatchOutcome::Delivered { attempts }
            }
            Err(attempts) => DispatchOutcome::Failed { attempts },
        }
    }

    /// Clear the dedup state and deliver the resolve with the retry policy. The resolve is
    /// delivered even when the key wasn't locally active — core may have restarted since the
    /// fire, and PagerDuty/JSM resolves are idempotent — so a remote incident is never left
    /// dangling open.
    pub async fn dispatch_resolve(&mut self, notification: Notification) -> DispatchOutcome {
        self.active.remove(&notification.dedup_key);
        match self.deliver_with_retry(&notification, true).await {
            Ok(attempts) => DispatchOutcome::Delivered { attempts },
            Err(attempts) => DispatchOutcome::Failed { attempts },
        }
    }

    /// Mark an alert resolved so the next occurrence notifies again.
    pub fn mark_resolved(&mut self, dedup_key: &DedupKey) {
        self.active.remove(dedup_key);
    }

    async fn deliver_with_retry(
        &self,
        notification: &Notification,
        resolve: bool,
    ) -> Result<u32, u32> {
        let max = self.policy.max_attempts.max(1);
        for attempt in 1..=max {
            let result = if resolve {
                self.channel.deliver_resolve(notification).await
            } else {
                self.channel.deliver(notification).await
            };
            match result {
                Ok(()) => return Ok(attempt),
                Err(_) if attempt < max => {
                    let backoff = self.policy.base_backoff_ms * (1u64 << (attempt - 1));
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                }
                Err(_) => return Err(attempt),
            }
        }
        Err(max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use yagra_common::{CheckId, NodeId, NodeState};

    /// A channel that fails its first `fail_first` calls, then succeeds. Counts calls.
    struct FlakyChannel {
        fail_first: u32,
        calls: Arc<AtomicU32>,
    }

    #[async_trait]
    impl NotifyChannel for FlakyChannel {
        async fn deliver(&self, _n: &Notification) -> Result<(), NotifyError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n <= self.fail_first {
                Err(NotifyError::Delivery("transient".into()))
            } else {
                Ok(())
            }
        }
    }

    fn notification() -> Notification {
        let alert = Alert {
            node: NodeId::new(),
            check: CheckId::new(),
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 0,
            root_cause: None,
            flapping: false,
            metric: "__liveness__".to_string(),
            breach: None,
        };
        Notification::for_alert(&alert, "node down", "{}")
    }

    fn no_backoff() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_backoff_ms: 0,
        }
    }

    #[tokio::test]
    async fn delivers_on_first_try() {
        let calls = Arc::new(AtomicU32::new(0));
        let mut d = Dispatcher::new(
            FlakyChannel {
                fail_first: 0,
                calls: calls.clone(),
            },
            no_backoff(),
        );
        assert_eq!(
            d.dispatch(notification()).await,
            DispatchOutcome::Delivered { attempts: 1 }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_then_succeeds() {
        let calls = Arc::new(AtomicU32::new(0));
        let mut d = Dispatcher::new(
            FlakyChannel {
                fail_first: 2,
                calls: calls.clone(),
            },
            no_backoff(),
        );
        assert_eq!(
            d.dispatch(notification()).await,
            DispatchOutcome::Delivered { attempts: 3 }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let calls = Arc::new(AtomicU32::new(0));
        let mut d = Dispatcher::new(
            FlakyChannel {
                fail_first: 99,
                calls: calls.clone(),
            },
            no_backoff(),
        );
        assert_eq!(
            d.dispatch(notification()).await,
            DispatchOutcome::Failed { attempts: 3 }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    /// A lifecycle-aware channel counting trigger and resolve deliveries separately.
    /// Resolve fails its first `resolve_fail_first` calls to exercise the retry path.
    struct LifecycleChannel {
        triggers: Arc<AtomicU32>,
        resolves: Arc<AtomicU32>,
        resolve_fail_first: u32,
    }

    #[async_trait]
    impl NotifyChannel for LifecycleChannel {
        async fn deliver(&self, _n: &Notification) -> Result<(), NotifyError> {
            self.triggers.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn deliver_resolve(&self, _n: &Notification) -> Result<(), NotifyError> {
            let n = self.resolves.fetch_add(1, Ordering::SeqCst) + 1;
            if n <= self.resolve_fail_first {
                Err(NotifyError::Delivery("transient".into()))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn resolve_default_is_noop_for_deliver_only_channels() {
        // FlakyChannel doesn't override deliver_resolve — the default no-op must succeed
        // without touching the channel's deliver path.
        let calls = Arc::new(AtomicU32::new(0));
        let mut d = Dispatcher::new(
            FlakyChannel {
                fail_first: 99, // deliver would fail; resolve must not call it
                calls: calls.clone(),
            },
            no_backoff(),
        );
        assert_eq!(
            d.dispatch_resolve(notification()).await,
            DispatchOutcome::Delivered { attempts: 1 }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn resolve_clears_dedup_and_delivers() {
        let triggers = Arc::new(AtomicU32::new(0));
        let resolves = Arc::new(AtomicU32::new(0));
        let mut d = Dispatcher::new(
            LifecycleChannel {
                triggers: triggers.clone(),
                resolves: resolves.clone(),
                resolve_fail_first: 0,
            },
            no_backoff(),
        );
        let n = notification();
        assert_eq!(
            d.dispatch(n.clone()).await,
            DispatchOutcome::Delivered { attempts: 1 }
        );
        assert_eq!(
            d.dispatch_resolve(n.clone()).await,
            DispatchOutcome::Delivered { attempts: 1 }
        );
        assert_eq!(resolves.load(Ordering::SeqCst), 1);
        // Dedup cleared by the resolve: the next fire pages again.
        assert_eq!(
            d.dispatch(n).await,
            DispatchOutcome::Delivered { attempts: 1 }
        );
        assert_eq!(triggers.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn resolve_delivers_even_when_not_active() {
        // Core may have restarted since the fire — the resolve still goes out (idempotent
        // on the PagerDuty/JSM side) so remote incidents are never left open.
        let resolves = Arc::new(AtomicU32::new(0));
        let mut d = Dispatcher::new(
            LifecycleChannel {
                triggers: Arc::new(AtomicU32::new(0)),
                resolves: resolves.clone(),
                resolve_fail_first: 0,
            },
            no_backoff(),
        );
        assert_eq!(
            d.dispatch_resolve(notification()).await,
            DispatchOutcome::Delivered { attempts: 1 }
        );
        assert_eq!(resolves.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resolve_retries_per_policy() {
        let resolves = Arc::new(AtomicU32::new(0));
        let mut d = Dispatcher::new(
            LifecycleChannel {
                triggers: Arc::new(AtomicU32::new(0)),
                resolves: resolves.clone(),
                resolve_fail_first: 2,
            },
            no_backoff(),
        );
        assert_eq!(
            d.dispatch_resolve(notification()).await,
            DispatchOutcome::Delivered { attempts: 3 }
        );
        assert_eq!(resolves.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn duplicate_active_alert_is_suppressed() {
        let calls = Arc::new(AtomicU32::new(0));
        let mut d = Dispatcher::new(
            FlakyChannel {
                fail_first: 0,
                calls: calls.clone(),
            },
            no_backoff(),
        );
        let n = notification();
        assert_eq!(
            d.dispatch(n.clone()).await,
            DispatchOutcome::Delivered { attempts: 1 }
        );
        // Same dedup key again → suppressed, channel not called.
        assert_eq!(d.dispatch(n.clone()).await, DispatchOutcome::Suppressed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // After resolve, it notifies again.
        d.mark_resolved(&n.dedup_key);
        assert_eq!(
            d.dispatch(n).await,
            DispatchOutcome::Delivered { attempts: 1 }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
