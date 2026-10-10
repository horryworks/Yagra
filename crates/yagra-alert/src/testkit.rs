// SPDX-License-Identifier: AGPL-3.0-only
//! Shared test fixtures for the three alert modules.
//!
//! These were plain helpers inside one `mod tests` until ADR-083 split the file. A private test
//! module cannot be reached from a sibling, so the twelve that more than one side needs live here
//! rather than being copied — a copied fixture is how two tests start disagreeing about what a
//! "liveness rule" is while both stay green.
//!
//! The `mod` declaration carries `#[cfg(any(test, feature = "test-util"))]`, so none of this reaches
//! a release binary: `yagra-core` turns the feature on from its dev-dependencies only, for the core
//! tests that drive the engine with the same fixtures (ADR-202 Inc.4).

use std::collections::HashMap;

use crate::{Alert, Subject};
use uuid::Uuid;
use yagra_bus::{CheckOutcome, PollResult};
use yagra_common::{
    resolve_effective, Direction, EffectiveThreshold, IfIndex, NodeId, ScopeLevel, ScopedThreshold,
};

use yagra_common::StoredThreshold;

use crate::engine::AlertManager;
use crate::rules::{
    folder_depth, nearest_folder_depth, seeded_liveness_rule, threshold_applies, AlertConfig,
    NodeMeta,
};

/// The fleet-default liveness rule every deployment is seeded with (ADR-075, `repo.rs`).
///
/// Up/down alerting is rule-driven now, so a manager with no config commits state and pages
/// nobody. Tests that exercise firing must install this; the ones that assert the opposite
/// deliberately leave it out.
pub fn liveness_rule() -> StoredThreshold {
    seeded_liveness_rule()
}

/// `AlertConfig::new` with the seeded liveness rule already in it — what a real deployment
/// looks like. Take this rather than `AlertConfig::new` unless the test is about its absence.
pub fn cfg(mut thresholds: Vec<StoredThreshold>, meta: HashMap<NodeId, NodeMeta>) -> AlertConfig {
    thresholds.push(liveness_rule());
    AlertConfig::new(thresholds, meta)
}

/// A manager configured the way a seeded deployment is.
pub fn manager() -> AlertManager {
    let mgr = AlertManager::new(untitled);
    mgr.set_config(cfg(Vec::new(), HashMap::new()));
    mgr
}

pub fn result(node: NodeId, outcome: CheckOutcome, at: i64) -> PollResult {
    PollResult::new(Uuid::nil(), node, at, outcome)
}

pub fn folder_rule(group: Uuid, warning: f64) -> StoredThreshold {
    StoredThreshold::new(
        Uuid::from_u128(u128::from(warning as u64) + 1),
        ScopeLevel::FolderGroup,
        vec![group.to_string()],
        yagra_common::ThresholdRule::new(
            "cpu_util",
            yagra_common::ThresholdBounds::above(Some(warning), None),
            1,
        ),
    )
}

pub fn in_folder(node: NodeId, chain: Vec<Uuid>) -> HashMap<NodeId, NodeMeta> {
    let mut meta = HashMap::new();
    meta.insert(
        node,
        NodeMeta {
            folder_group: chain.first().copied(),
            folder_chain: chain,
            ..NodeMeta::default()
        },
    );
    meta
}

/// The **reference implementation**: `AlertConfig::resolve`'s body exactly as it stood before
/// the rules were indexed, working from the flat per-metric list in its original order.
///
/// 🚨 **Do not "improve" this.** Its whole value is being the slow, obvious version — the one
/// that scans every rule and asks `threshold_applies` about each. If it is ever optimised to
/// resemble the indexed implementation, the differential test below stops comparing two
/// things and starts comparing one thing to itself.
pub fn resolve_reference(
    candidates: &[StoredThreshold],
    node: NodeId,
    ifindex: Option<IfIndex>,
    meta: Option<&NodeMeta>,
) -> Option<EffectiveThreshold> {
    let matched: Vec<&StoredThreshold> = candidates
        .iter()
        // No row name: the differential test compares port and node resolution, where none applies.
        .filter(|t| threshold_applies(t, node, ifindex, None, meta))
        .collect();
    let nearest = nearest_folder_depth(&matched, meta);
    let scoped: Vec<ScopedThreshold> = matched
        .into_iter()
        .filter(|t| t.level != ScopeLevel::FolderGroup || folder_depth(t, meta) == nearest)
        .map(|t| ScopedThreshold::new(t.level, t.rule.clone()))
        .collect();
    resolve_effective(&scoped)
}

pub fn rule_at(level: ScopeLevel, scope_id: &str, dir: Direction, crit: f64) -> StoredThreshold {
    StoredThreshold::new(
        Uuid::new_v4(),
        level,
        vec![scope_id.to_string()],
        yagra_common::ThresholdRule::new(
            "cpu_util",
            yagra_common::ThresholdBounds::from_legacy(dir, None, Some(crit)),
            3,
        ),
    )
}

/// The same, naming several targets at once (ADR-078).
pub fn rule_at_many(level: ScopeLevel, ids: &[&str], dir: Direction, crit: f64) -> StoredThreshold {
    StoredThreshold::new(
        Uuid::new_v4(),
        level,
        ids.iter().map(|s| (*s).to_string()).collect(),
        yagra_common::ThresholdRule::new(
            "cpu_util",
            yagra_common::ThresholdBounds::from_legacy(dir, None, Some(crit)),
            3,
        ),
    )
}

pub fn meta_for(node: NodeId) -> HashMap<NodeId, NodeMeta> {
    let mut m = HashMap::new();
    m.insert(node, NodeMeta::default());
    m
}

/// One open critical alert about `node`'s `metric`, shaped the way `restore` reads one back out of
/// `alert_history` — which is the only way a test can start from "this was already open".
pub fn open_alert(node: NodeId, metric: &str, state: yagra_common::NodeState) -> Alert {
    Alert {
        subject: Subject::Node(node),
        check: crate::rules::check_id(node, metric),
        severity: yagra_common::Severity::Critical,
        state,
        at_unix_ms: 1_000,
        root_cause: None,
        flapping: false,
        metric: metric.to_owned(),
        breach: None,
        ifindex: None,
        row: None,
        row_name: None,
    }
}

/// A port-scoped `if_in_util_pct above <warning>` rule, dwell 1.
pub fn port_rule(node: NodeId, idx: IfIndex, warning: f64) -> StoredThreshold {
    use yagra_common::{ThresholdBounds, ThresholdRule};
    StoredThreshold::new(
        Uuid::nil(),
        ScopeLevel::Interface,
        vec![format!("{node}:{}", idx.0)],
        ThresholdRule::new(
            "if_in_util_pct",
            ThresholdBounds::above(Some(warning), Some(90.0)),
            1,
        ),
    )
}

/// The gate `run_interface_utilization_watch` applies, assembled from its two halves so a test
/// asks exactly the question the loop asks.
pub fn may_observe(mgr: &AlertManager, node: NodeId) -> bool {
    crate::engine::may_observe_ports(mgr.node_liveness(node))
}

/// The title source of a manager whose frames a test does not read: no title.
pub fn untitled(_: &str) -> Option<String> {
    None
}

/// A title source a test can recognise, standing in for core's `metric_meaning::alert_title_of`.
pub fn titled(metric: &str) -> Option<String> {
    Some(format!("title of {metric}"))
}
