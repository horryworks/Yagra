// SPDX-License-Identifier: AGPL-3.0-only
//! Filling a notification template's context (ADR-039).
//!
//! An [`Alert`] carries ids: node, check, severity, state, the breach. That is exactly right for
//! the engine — names change, ids do not — and exactly wrong for a notification, where
//! `node 6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60 is critical` is what an operator was actually being
//! sent. Resolving those ids is what makes `{{ node_name }}` possible, and a template feature
//! without it would not have addressed the complaint that motivated ADR-039.
//!
//! The lookup sits behind [`AlertFactsSource`] rather than being a direct `NodeRepo` call so the
//! notifier can be unit-tested without a database, and so a core running in skeleton mode (no write
//! side) simply has no source and falls back to ids.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;
use yagra_alert::Alert;
use yagra_common::{AlertFacts, NotifyEvent};

use crate::repo::{NodeFacts, NodeRepo};

/// How long a resolved node's facts stay usable.
///
/// Short enough that a rename shows up in the next notification, long enough that a fleet-wide
/// outage does not become one query per alert through the single, serialized notify worker.
const FACTS_TTL: Duration = Duration::from_secs(60);
/// Cache ceiling. Past this the cache is cleared rather than evicted one-by-one: it is a
/// short-lived hot set, not a working set worth an LRU, and a cleared cache costs one query.
const FACTS_CAPACITY: usize = 4096;

/// Resolves the display facts a notification template needs.
#[async_trait]
pub trait AlertFactsSource: Send + Sync {
    /// Facts for each requested node. Ids that cannot be resolved are simply absent.
    async fn facts(&self, ids: &[Uuid]) -> HashMap<Uuid, NodeFacts>;
}

/// [`NodeRepo`]-backed source with a short TTL cache.
pub struct CachedNodeFacts {
    repo: Arc<NodeRepo>,
    // A plain Mutex, not an async one: every critical section here is a map lookup with no await
    // inside it, so holding it across a scheduling point is impossible by construction.
    cache: Mutex<HashMap<Uuid, (NodeFacts, Instant)>>,
}

impl CachedNodeFacts {
    /// Wrap the node repository.
    #[must_use]
    pub fn new(repo: Arc<NodeRepo>) -> Self {
        Self {
            repo,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl AlertFactsSource for CachedNodeFacts {
    async fn facts(&self, ids: &[Uuid]) -> HashMap<Uuid, NodeFacts> {
        let now = Instant::now();
        let mut out = HashMap::with_capacity(ids.len());
        let mut missing = Vec::new();
        {
            let cache = self.cache.lock().expect("notify facts cache poisoned");
            for id in ids {
                match cache.get(id) {
                    Some((facts, at)) if now.duration_since(*at) < FACTS_TTL => {
                        out.insert(*id, facts.clone());
                    }
                    _ => missing.push(*id),
                }
            }
        }
        if missing.is_empty() {
            return out;
        }
        // A failure here degrades to ids in the rendered notification. It must never stop the
        // notification: the alert is the point, the name is a courtesy.
        let fetched = self.repo.node_facts(&missing).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to resolve node facts for a notification");
            HashMap::new()
        });
        {
            let mut cache = self.cache.lock().expect("notify facts cache poisoned");
            if cache.len().saturating_add(fetched.len()) > FACTS_CAPACITY {
                cache.clear();
            }
            for (id, facts) in &fetched {
                cache.insert(*id, (facts.clone(), now));
            }
        }
        out.extend(fetched);
        out
    }
}

/// Build the template context for one alert.
///
/// `metric` deliberately drops the liveness sentinel: `__liveness__` is an internal check name, not
/// a metric, and printing it into an operator's inbox is the same mistake `rca/prompt.rs` avoids
/// when building an LLM prompt. An up/down alert simply leaves `metric` undefined.
#[must_use]
pub fn context_for(
    alert: &Alert,
    event: NotifyEvent,
    resolved: &HashMap<Uuid, NodeFacts>,
) -> AlertFacts {
    let node_id = alert.node.as_uuid();
    let node = resolved.get(&node_id);
    let root = alert.root_cause.map(|r| r.as_uuid());
    AlertFacts {
        event,
        node_id: node_id.to_string(),
        node_name: node.map_or_else(|| node_id.to_string(), |f| f.name.clone()),
        node_address: node.map(|f| f.address.clone()),
        group: node.and_then(|f| f.group.clone()),
        profile: node.and_then(|f| f.profile.clone()),
        check_id: alert.check.as_uuid().to_string(),
        dedup_key: crate::alerts::dedup_string(&alert.dedup_key()),
        severity: alert.severity.as_str().to_owned(),
        state: alert.state.as_str().to_owned(),
        metric: (alert.metric != crate::alerts::LIVENESS && !alert.metric.is_empty())
            .then(|| alert.metric.clone()),
        value: alert.breach.as_ref().map(|b| b.value),
        threshold: alert.breach.as_ref().and_then(|b| b.threshold),
        direction: alert
            .breach
            .as_ref()
            .map(|b| b.direction.as_str().to_owned()),
        at: unix_ms_to_rfc3339(alert.at_unix_ms),
        at_unix_ms: alert.at_unix_ms,
        flapping: alert.flapping,
        root_cause_id: root.map(|r| r.to_string()),
        root_cause_name: root.map(|r| {
            resolved
                .get(&r)
                .map_or_else(|| r.to_string(), |f| f.name.clone())
        }),
    }
}

/// The alert the template preview renders against, with its node facts already resolved.
///
/// Returned as a real [`Alert`] rather than a ready-made context so the preview runs the *same*
/// [`context_for`] and built-in-text code the delivery path runs. A preview that agreed with a
/// separate copy of the rules would be worth nothing — it exists precisely to tell an operator what
/// will actually be sent.
///
/// The values are chosen to reproduce [`yagra_common::sample_facts`] exactly, which
/// `the_preview_sample_is_the_declared_sample` pins.
#[must_use]
pub fn preview_sample() -> (Alert, HashMap<Uuid, NodeFacts>) {
    let declared = yagra_common::sample_facts(NotifyEvent::Fire);
    let node: Uuid = declared.node_id.parse().expect("sample node id is a uuid");
    let root: Uuid = declared
        .root_cause_id
        .as_deref()
        .expect("the sample is rolled up")
        .parse()
        .expect("sample root-cause id is a uuid");
    let alert = Alert {
        node: yagra_common::NodeId::from(node),
        check: yagra_common::CheckId::from(
            declared
                .check_id
                .parse::<Uuid>()
                .expect("sample check id is a uuid"),
        ),
        severity: yagra_common::Severity::from_token(&declared.severity)
            .expect("sample severity is a token"),
        state: yagra_common::NodeState::from_token(&declared.state).expect("sample state is valid"),
        at_unix_ms: declared.at_unix_ms,
        root_cause: Some(yagra_common::NodeId::from(root)),
        flapping: declared.flapping,
        metric: declared.metric.clone().unwrap_or_default(),
        breach: declared.value.map(|value| yagra_alert::Breach {
            value,
            threshold: declared.threshold,
            direction: declared
                .direction
                .as_deref()
                .and_then(yagra_common::Direction::from_token)
                .unwrap_or(yagra_common::Direction::Above),
        }),
    };
    let mut resolved = HashMap::new();
    resolved.insert(
        node,
        NodeFacts {
            name: declared.node_name.clone(),
            address: declared.node_address.clone().unwrap_or_default(),
            group: declared.group.clone(),
            profile: declared.profile.clone(),
        },
    );
    resolved.insert(
        root,
        NodeFacts {
            name: declared.root_cause_name.clone().unwrap_or_default(),
            address: String::new(),
            group: None,
            profile: None,
        },
    );
    (alert, resolved)
}

/// Every node id one alert's context needs resolved — itself, plus its root cause when rolled up.
#[must_use]
pub fn node_ids_for(alert: &Alert) -> Vec<Uuid> {
    let mut ids = vec![alert.node.as_uuid()];
    if let Some(root) = alert.root_cause {
        ids.push(root.as_uuid());
    }
    ids
}

/// Unix milliseconds → RFC 3339 UTC, falling back to the epoch rather than panicking (same
/// discipline as `mcp/dto.rs`: a malformed timestamp must not take down a notification).
fn unix_ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).expect("epoch is valid"))
        .to_rfc3339()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use yagra_alert::Breach;
    use yagra_common::{CheckId, Direction, NodeId, NodeState, Severity};

    /// A source that answers from a fixed table and counts calls.
    pub(crate) struct FakeFacts {
        pub rows: HashMap<Uuid, NodeFacts>,
        pub calls: AtomicUsize,
    }

    impl FakeFacts {
        pub(crate) fn with(id: Uuid, name: &str) -> Self {
            let mut rows = HashMap::new();
            rows.insert(
                id,
                NodeFacts {
                    name: name.to_owned(),
                    address: "192.0.2.7".to_owned(),
                    group: Some("Tokyo".to_owned()),
                    profile: Some("Cisco switch".to_owned()),
                },
            );
            Self {
                rows,
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl AlertFactsSource for FakeFacts {
        async fn facts(&self, ids: &[Uuid]) -> HashMap<Uuid, NodeFacts> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            ids.iter()
                .filter_map(|id| self.rows.get(id).map(|f| (*id, f.clone())))
                .collect()
        }
    }

    pub(crate) fn threshold_alert(node: NodeId) -> Alert {
        Alert {
            node,
            check: CheckId::new(),
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 1_785_836_467_000,
            root_cause: None,
            flapping: false,
            metric: "icmp_rtt_ms".to_owned(),
            breach: Some(Breach {
                value: 412.5,
                threshold: Some(200.0),
                direction: Direction::Above,
            }),
        }
    }

    fn liveness_alert(node: NodeId) -> Alert {
        Alert {
            metric: crate::alerts::LIVENESS.to_owned(),
            breach: None,
            state: NodeState::Unreachable,
            ..threshold_alert(node)
        }
    }

    #[tokio::test]
    async fn a_resolved_node_supplies_its_name_group_and_profile() {
        let node = NodeId::new();
        let src = FakeFacts::with(node.as_uuid(), "core-sw-01");
        let alert = threshold_alert(node);
        let resolved = src.facts(&node_ids_for(&alert)).await;
        let ctx = context_for(&alert, NotifyEvent::Fire, &resolved);
        assert_eq!(ctx.node_name, "core-sw-01");
        assert_eq!(ctx.node_address.as_deref(), Some("192.0.2.7"));
        assert_eq!(ctx.group.as_deref(), Some("Tokyo"));
        assert_eq!(ctx.profile.as_deref(), Some("Cisco switch"));
        assert_eq!(ctx.metric.as_deref(), Some("icmp_rtt_ms"));
        assert_eq!(ctx.value, Some(412.5));
        assert_eq!(ctx.direction.as_deref(), Some("above"));
        assert_eq!(ctx.at, "2026-08-04T09:41:07+00:00");
    }

    /// The preview sample hands the operator `at` and `at_unix_ms` side by side. They are two
    /// hand-written literals in `yagra-common` (which has no clock to derive one from the other),
    /// so this is what stops them drifting into describing different instants.
    /// `yagra-common` declares the sample; `yagra-core` builds an `Alert` that has to reproduce it
    /// so the preview can run the real rendering path. Two hand-written shapes describing one
    /// alert, so they get a test rather than a comment.
    #[test]
    fn the_preview_sample_is_the_declared_sample() {
        let (alert, resolved) = preview_sample();
        for event in NotifyEvent::ALL {
            assert_eq!(
                context_for(&alert, event, &resolved),
                yagra_common::sample_facts(event),
                "the preview's alert no longer reproduces sample_facts"
            );
        }
    }

    #[test]
    fn the_preview_sample_states_one_instant_two_ways() {
        let sample = yagra_common::sample_facts(NotifyEvent::Fire);
        let from_ms = chrono::DateTime::from_timestamp_millis(sample.at_unix_ms).expect("in range");
        let from_text = chrono::DateTime::parse_from_rfc3339(&sample.at).expect("valid RFC 3339");
        assert_eq!(from_ms, from_text, "sample_facts disagrees with itself");
    }

    /// The degraded path has to stay usable: a notification with a raw id in it is far better than
    /// no notification, so an unresolvable node falls back to its id rather than blanking.
    #[tokio::test]
    async fn an_unresolvable_node_falls_back_to_its_id() {
        let node = NodeId::new();
        let alert = threshold_alert(node);
        let ctx = context_for(&alert, NotifyEvent::Fire, &HashMap::new());
        assert_eq!(ctx.node_name, node.as_uuid().to_string());
        assert_eq!(ctx.node_id, node.as_uuid().to_string());
        assert!(ctx.node_address.is_none());
        assert!(ctx.group.is_none());
    }

    /// `__liveness__` is an internal check name. It must not reach a template, or every up/down
    /// notification that interpolates `{{ metric }}` prints it.
    #[tokio::test]
    async fn a_liveness_alert_exposes_no_metric_and_no_breach() {
        let node = NodeId::new();
        let ctx = context_for(&liveness_alert(node), NotifyEvent::Fire, &HashMap::new());
        assert!(ctx.metric.is_none(), "the sentinel must not be exposed");
        assert!(ctx.value.is_none());
        assert!(ctx.threshold.is_none());
        assert!(ctx.direction.is_none());
        assert_eq!(ctx.state, "unreachable");
    }

    #[tokio::test]
    async fn a_rolled_up_alert_resolves_its_root_cause_too() {
        let node = NodeId::new();
        let root = NodeId::new();
        let mut src = FakeFacts::with(node.as_uuid(), "leaf-sw");
        src.rows.insert(
            root.as_uuid(),
            NodeFacts {
                name: "edge-rtr-01".to_owned(),
                address: "192.0.2.1".to_owned(),
                group: None,
                profile: None,
            },
        );
        let alert = Alert {
            root_cause: Some(root),
            ..threshold_alert(node)
        };
        let ids = node_ids_for(&alert);
        assert_eq!(ids.len(), 2, "the root cause has to be fetched as well");
        let ctx = context_for(&alert, NotifyEvent::Suppress, &src.facts(&ids).await);
        assert_eq!(ctx.root_cause_name.as_deref(), Some("edge-rtr-01"));
        assert_eq!(
            ctx.root_cause_id.as_deref(),
            Some(root.to_string()).as_deref()
        );
    }

    /// The dedup key in the template is the same string the vendors receive, so an operator can
    /// correlate what Yagra sent with what PagerDuty/JSM shows.
    #[tokio::test]
    async fn the_context_dedup_key_matches_the_vendor_one() {
        let node = NodeId::new();
        let alert = threshold_alert(node);
        let ctx = context_for(&alert, NotifyEvent::Fire, &HashMap::new());
        assert_eq!(
            ctx.dedup_key,
            crate::alerts::dedup_string(&alert.dedup_key())
        );
        assert!(ctx.dedup_key.starts_with("yagra:"));
    }

    #[tokio::test]
    async fn an_out_of_range_timestamp_degrades_instead_of_panicking() {
        let alert = Alert {
            at_unix_ms: i64::MAX,
            ..threshold_alert(NodeId::new())
        };
        let ctx = context_for(&alert, NotifyEvent::Fire, &HashMap::new());
        assert!(ctx.at.starts_with("1970-01-01"));
    }
}
