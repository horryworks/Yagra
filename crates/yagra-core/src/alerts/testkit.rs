// SPDX-License-Identifier: AGPL-3.0-only
//! Shared test fixtures for the three alert modules.
//!
//! These were plain helpers inside one `mod tests` until ADR-083 split the file. A private test
//! module cannot be reached from a sibling, so the twelve that more than one side needs live here
//! rather than being copied — a copied fixture is how two tests start disagreeing about what a
//! "liveness rule" is while both stay green.
//!
//! The `mod` declaration carries `#[cfg(test)]`, so none of this reaches a release binary.

use std::collections::HashMap;

use uuid::Uuid;
use yagra_bus::{CheckOutcome, PollResult};
use yagra_common::{
    resolve_effective, Direction, EffectiveThreshold, IfIndex, NodeId, ScopeLevel, ScopedThreshold,
};

use crate::thresholds::StoredThreshold;

use super::rules::{folder_depth, nearest_folder_depth, seeded_liveness_rule, threshold_applies};
use super::{AlertConfig, AlertManager, NodeMeta};

/// The fleet-default liveness rule every deployment is seeded with (ADR-075, `repo.rs`).
///
/// Up/down alerting is rule-driven now, so a manager with no config commits state and pages
/// nobody. Tests that exercise firing must install this; the ones that assert the opposite
/// deliberately leave it out.
pub(crate) fn liveness_rule() -> StoredThreshold {
    seeded_liveness_rule()
}

/// `AlertConfig::new` with the seeded liveness rule already in it — what a real deployment
/// looks like. Take this rather than `AlertConfig::new` unless the test is about its absence.
pub(crate) fn cfg(
    mut thresholds: Vec<StoredThreshold>,
    meta: HashMap<NodeId, NodeMeta>,
) -> AlertConfig {
    thresholds.push(liveness_rule());
    AlertConfig::new(thresholds, meta)
}

/// A manager configured the way a seeded deployment is.
pub(crate) fn manager() -> AlertManager {
    let mgr = AlertManager::new();
    mgr.set_config(cfg(Vec::new(), HashMap::new()));
    mgr
}

pub(crate) fn result(node: NodeId, outcome: CheckOutcome, at: i64) -> PollResult {
    PollResult {
        job_id: Uuid::nil(),
        node_id: node,
        at_unix_ms: at,
        outcome,
        samples: Vec::new(),
        interfaces: Vec::new(),
        sys_descr: None,
        dns_chain: None,
        neighbors: None,
        l3: None,
        arp: None,
        routing: None,
        observational: false,
        poller_id: None,
        trace_context: Default::default(),
    }
}

pub(crate) fn folder_rule(group: Uuid, warning: f64) -> StoredThreshold {
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

pub(crate) fn in_folder(node: NodeId, chain: Vec<Uuid>) -> HashMap<NodeId, NodeMeta> {
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
pub(crate) fn resolve_reference(
    candidates: &[StoredThreshold],
    node: NodeId,
    ifindex: Option<IfIndex>,
    meta: Option<&NodeMeta>,
) -> Option<EffectiveThreshold> {
    let matched: Vec<&StoredThreshold> = candidates
        .iter()
        .filter(|t| threshold_applies(t, node, ifindex, meta))
        .collect();
    let nearest = nearest_folder_depth(&matched, meta);
    let scoped: Vec<ScopedThreshold> = matched
        .into_iter()
        .filter(|t| t.level != ScopeLevel::FolderGroup || folder_depth(t, meta) == nearest)
        .map(|t| ScopedThreshold::new(t.level, t.rule.clone()))
        .collect();
    resolve_effective(&scoped)
}

pub(crate) fn rule_at(
    level: ScopeLevel,
    scope_id: &str,
    dir: Direction,
    crit: f64,
) -> StoredThreshold {
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
pub(crate) fn rule_at_many(
    level: ScopeLevel,
    ids: &[&str],
    dir: Direction,
    crit: f64,
) -> StoredThreshold {
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

pub(crate) fn meta_for(node: NodeId) -> HashMap<NodeId, NodeMeta> {
    let mut m = HashMap::new();
    m.insert(node, NodeMeta::default());
    m
}

/// A port-scoped `if_in_util_pct above <warning>` rule, dwell 1.
pub(crate) fn port_rule(node: NodeId, idx: IfIndex, warning: f64) -> StoredThreshold {
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
pub(crate) fn may_observe(mgr: &AlertManager, node: NodeId) -> bool {
    crate::interface_util::may_observe_ports(mgr.node_liveness(node))
}
