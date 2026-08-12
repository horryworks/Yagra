// SPDX-License-Identifier: AGPL-3.0-only
//! What the model is told about an incident (ADR-029).
//!
//! Gathering is split from rendering on purpose: this module produces a plain
//! [`IncidentContext`] — no strings formatted for a prompt, no vendor in sight — and
//! [`super::prompt`] turns that into text. The split is what makes the interesting parts testable
//! without a database, and it keeps the "what may the model see" decision in one reviewable place.
//!
//! **Three rules govern what goes in.**
//!
//! 1. **Explain the incident, not the symptom.** If the alert the operator clicked was rolled up
//!    under an upstream failure, the context is assembled for that upstream node instead. Forty
//!    servers behind a dead switch are one incident with one cause, and the roll-up already knows
//!    which node it is.
//! 2. **No credentials, by construction.** Nothing here touches [`crate::secrets::CredentialStore`],
//!    and [`NodeFacts::from_node`] lists the fields it copies **explicitly** — no `..rest`. A future
//!    secret-bearing field on [`Node`] therefore cannot reach a prompt by being added upstream; it
//!    has to be added here, in a diff someone reviews. A canary test holds that line.
//! 3. **Everything is bounded.** A cascade can touch thousands of nodes and a chatty device can log
//!    hundreds of lines a minute; every list below has a cap, and the counts survive truncation so
//!    the model is told "20 of 380" rather than being quietly handed 20.
//!
//! What is deliberately *not* here: interface lists, full metric series, and configuration bodies.
//! They are large, and the timeline already carries the part that moved.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::IpAddr;

use serde::Serialize;
use uuid::Uuid;
use yagra_alert::Alert;
use yagra_common::{Direction, Node, NodeId};

use crate::alerts::AlertManager;
use crate::analysis::{AnalysisRunner, IncidentSignal};
use crate::audit::AuditRepo;
use crate::repo::NodeRepo;

/// Dependents named individually before the rest become a count. Twenty is enough for the model to
/// see the shape of a cascade ("all of rack 3") without the list crowding out the timeline.
const MAX_NAMED_DEPENDENTS: usize = 20;
/// How far up the inventory to walk. A depth cap rather than a visited-set because the inventory is
/// a tree; the cap is belt-and-braces against a cycle introduced by a bad edit.
const MAX_UPSTREAM_DEPTH: usize = 8;
/// Config changes carried into the context. "Someone changed this 20 minutes ago" is high-signal;
/// the twenty-first is not.
const MAX_RECENT_CHANGES: usize = 5;
/// Audit rows scanned to find those changes. The audit table has no `node_id` column — the action
/// is free text like `PUT /api/v1/nodes/<uuid>` — so the node is matched by substring. Scanning a
/// bounded, index-ordered window keeps that from becoming a sequential scan of the whole table.
const AUDIT_SCAN_ROWS: i64 = 200;
/// Tags carried per node. Operator-set metadata (site, role) is genuinely useful for diagnosis and
/// is already in every API response; the cap just stops a heavily-tagged node from dominating.
const MAX_TAGS: usize = 8;

/// Exactly what the model was shown about the incident, so an answer can be checked rather than
/// trusted (ADR-029). Stored beside the answer; also what makes a bad answer reviewable later.
//  Below the doc line: a schema's doc text is published verbatim to API clients, so keep the
//  gathering rules (the three in the module docs) out of it.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct IncidentContext {
    /// When the context was assembled (Unix seconds), so the prompt can express ages rather than
    /// absolute times the model has no clock for.
    pub generated_at_s: i64,
    /// How far back the timeline reaches.
    pub window_secs: i64,
    /// Id of the node being explained. Differs from the one the operator clicked whenever the
    /// roll-up hop fired, and it is what a generated report is filed under — a report explains the
    /// cause, so it belongs to the cause.
    pub root_node_id: Uuid,
    /// The node being explained — the root cause, after any roll-up hop.
    pub node: NodeFacts,
    /// The alert on that node.
    pub alert: AlertFacts,
    /// Alerts rolled up under this one.
    pub dependents: Dependents,
    /// Inventory ancestors, nearest first. Present even when healthy: "the parent is fine" is
    /// evidence that the failure is local to this node.
    pub upstream: Vec<NodeFacts>,
    /// Cross-signal timeline for the window, oldest first.
    pub timeline: Vec<IncidentSignal>,
    /// Recent audited configuration changes touching this node.
    pub recent_changes: Vec<ChangeFacts>,
}

impl IncidentContext {
    /// A canonical rendering of the *evidence*, used as the cache key for a generated report.
    ///
    /// Two things are deliberately left out, and both matter.
    ///
    /// **When the context was built.** `generated_at_s` and `window_secs` are properties of the
    /// request, not of the incident. Including them would make every click a cache miss, which is
    /// the entire failure this key exists to prevent: an operator reopening the same modal would be
    /// billed again for a differently-worded answer to an identical question.
    ///
    /// **Anomaly scores.** A signal's `severity` is recomputed over a window that slides with the
    /// clock, so it wobbles slightly between two calls about the same outage. Its `at_s`, `kind` and
    /// `label` do not. Keying on the wobble would defeat the cache without changing what the model
    /// is actually told.
    ///
    /// Everything that *would* change the answer is in: a new dependent, a new log line, a
    /// configuration change, a different node. So an incident that grows correctly misses the cache.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        // Versioned so a future change to what goes in invalidates old keys rather than colliding
        // with them (the same reason `dns_chains.chain_key` carries a `v1` line).
        let mut s = String::with_capacity(1024);
        s.push_str("v1\n");
        let _ = writeln!(s, "root {}", self.root_node_id);
        let _ = writeln!(s, "node {}", fp_node(&self.node));
        let a = &self.alert;
        let _ = writeln!(
            s,
            "alert {} {} {} {} {}",
            a.severity, a.state, a.metric, a.at_unix_ms, a.flapping
        );
        if let Some(b) = &a.breach {
            let _ = writeln!(s, "breach {} {:?} {}", b.value, b.threshold, b.direction);
        }
        if let Some(sym) = &a.asked_about {
            let _ = writeln!(s, "asked {sym}");
        }
        let _ = writeln!(
            s,
            "dependents {} {}",
            self.dependents.total,
            self.dependents.named.join(",")
        );
        for up in &self.upstream {
            let _ = writeln!(s, "up {}", fp_node(up));
        }
        for sig in &self.timeline {
            let _ = writeln!(s, "sig {} {} {}", sig.at_s, sig.kind, sig.label);
        }
        for c in &self.recent_changes {
            let _ = writeln!(s, "chg {} {} {} {}", c.at, c.username, c.action, c.status);
        }
        s
    }
}

/// One node's identity for [`IncidentContext::fingerprint`]. Tags are already sorted by the
/// `BTreeMap` they came from, so this is stable.
fn fp_node(n: &NodeFacts) -> String {
    let tags = n
        .tags
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{} {} {:?} {:?} {:?} [{}]",
        n.name, n.address, n.vendor, n.model, n.pool, tags
    )
}

/// The subset of a node the model may see. Notably not the credential binding.
//  Every field is copied by name in `from_node`. `credential` is **not** among them: it is only an
//  id, but there is no diagnostic use for it and excluding it keeps the rule simple enough to
//  enforce — nothing credential-shaped crosses this boundary. Kept below the doc line because this
//  struct's doc text is published verbatim to API clients (ADR-035).
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct NodeFacts {
    pub name: String,
    // utoipa has no schema for `IpAddr`; on the wire it is the serde `Display` form.
    #[schema(value_type = String)]
    pub address: IpAddr,
    pub vendor: Option<String>,
    pub model: Option<String>,
    /// Poller pool — usually the site, which is often the diagnosis ("everything in branch-osaka").
    pub pool: Option<String>,
    /// Operator-set grouping attributes, capped and sorted for a deterministic prompt.
    pub tags: Vec<(String, String)>,
}

impl NodeFacts {
    /// Copy the visible fields of a node.
    ///
    /// Written as an explicit field list rather than a struct update so that adding a field to
    /// [`Node`] cannot silently widen what a prompt contains — see the module docs.
    #[must_use]
    pub fn from_node(node: &Node) -> Self {
        Self {
            name: node.name.clone(),
            address: node.address,
            vendor: node.vendor.clone(),
            model: node.model.clone(),
            pool: node.pool.clone(),
            tags: cap_tags(&node.tags),
        }
    }
}

/// Take the first [`MAX_TAGS`] tags. `BTreeMap` iteration is already sorted, so the same node
/// renders the same way every time — a prompt that changes between calls for no reason is a
/// prompt-cache miss and an unexplainable diff.
fn cap_tags(tags: &BTreeMap<String, String>) -> Vec<(String, String)> {
    tags.iter()
        .take(MAX_TAGS)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The alert being explained.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct AlertFacts {
    pub severity: String,
    pub state: String,
    /// What the check measured — `icmp_rtt_ms`, or the liveness sentinel.
    pub metric: String,
    pub at_unix_ms: i64,
    /// Whether the engine is damping this check as flapping. Worth telling the model: a flapping
    /// link and a hard failure have different causes and different next steps.
    pub flapping: bool,
    /// Numeric detail for a threshold alert; absent for liveness.
    pub breach: Option<BreachFacts>,
    /// Set when the operator clicked a *symptom* and the context hopped to its cause. Naming the
    /// node they clicked keeps the answer connected to what they were looking at.
    pub asked_about: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct BreachFacts {
    pub value: f64,
    pub threshold: Option<f64>,
    pub direction: Direction,
}

/// Alerts attributed upstream to this incident.
#[derive(Debug, Clone, Default, PartialEq, Serialize, utoipa::ToSchema)]
pub struct Dependents {
    /// The named dependents, capped — see `total` for how many there actually are.
    pub named: Vec<String>,
    /// How many there are in total — kept separately so truncation is visible rather than silent.
    pub total: usize,
}

/// One audited configuration change.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct ChangeFacts {
    pub at: String,
    pub username: String,
    /// `"{METHOD} {path}"`, as recorded by the audit middleware.
    pub action: String,
    pub status: i32,
}

/// The stores the gatherer reads. Grouped into one struct so the call site stays readable and so a
/// future source is added in one place.
pub struct Sources<'a> {
    pub nodes: &'a NodeRepo,
    pub alerts: &'a AlertManager,
    pub analysis: &'a AnalysisRunner,
    pub audit: &'a AuditRepo,
}

/// Assemble the context for the incident containing `node`/`check`.
///
/// Resolves the root cause first (rule 1 in the module docs), then gathers around *that* node.
/// Every source is best-effort: a store that is unavailable contributes nothing rather than failing
/// the request, because a partial explanation beats no explanation. The one exception is the node
/// itself — without it there is no incident to describe.
///
/// # Errors
/// Returns operator-facing text when the node cannot be read or does not exist.
pub async fn gather(
    src: &Sources<'_>,
    node: NodeId,
    check: yagra_common::CheckId,
    window_secs: i64,
    sensitivity: f64,
    now_s: i64,
) -> Result<IncidentContext, String> {
    let active = src.alerts.active_alerts();
    let asked = active
        .iter()
        .find(|a| a.subject.is_node(node) && a.check == check)
        .cloned();

    // Rule 1: hop to the cause. The alert's own `root_cause` is what dependency suppression already
    // decided, so this agrees with the notification the operator received.
    let root_id = asked.as_ref().and_then(|a| a.root_cause).unwrap_or(node);
    let asked_about_symptom = root_id != node;

    let root_node = src
        .nodes
        .get_node(uuid_of(root_id))
        .await
        .map_err(|e| format!("could not read the node: {e}"))?
        .ok_or_else(|| "that node no longer exists".to_owned())?;

    // The alert to describe is the root's own. If the hop happened, the symptom alert we started
    // from is only used to say which node the operator clicked.
    let root_alert = if asked_about_symptom {
        active.iter().find(|a| a.subject.is_node(root_id)).cloned()
    } else {
        asked.clone()
    };
    let symptom_name = if asked_about_symptom {
        // Best-effort: the symptom node's name is a nicety, not worth failing the request for.
        src.nodes
            .get_node(uuid_of(node))
            .await
            .ok()
            .flatten()
            .map(|n| n.name)
    } else {
        None
    };

    let alert = root_alert.as_ref().map_or_else(
        || AlertFacts {
            // The alert cleared between the click and this call. Say so plainly rather than
            // inventing a severity — "it just recovered" is itself useful to the reader.
            severity: "cleared".to_owned(),
            state: "ok".to_owned(),
            metric: String::new(),
            at_unix_ms: now_s * 1000,
            flapping: false,
            breach: None,
            asked_about: symptom_name.clone(),
        },
        |a| alert_facts(a, symptom_name.clone()),
    );

    let dependents = roll_up_dependents(&active, root_id, &names_of(src, &active).await);
    let upstream = upstream_chain(src.nodes, &root_node).await;
    let timeline = src
        .analysis
        .incident_signals(
            uuid_of(root_id),
            now_s - window_secs,
            now_s,
            window_secs.max(600),
            sensitivity.max(2.0),
        )
        .await;
    let recent_changes = recent_changes(src.audit, uuid_of(root_id)).await;

    Ok(IncidentContext {
        generated_at_s: now_s,
        window_secs,
        root_node_id: uuid_of(root_id),
        node: NodeFacts::from_node(&root_node),
        alert,
        dependents,
        upstream,
        timeline,
        recent_changes,
    })
}

fn uuid_of(id: NodeId) -> Uuid {
    id.0
}

fn alert_facts(a: &Alert, asked_about: Option<String>) -> AlertFacts {
    AlertFacts {
        severity: a.severity.as_str().to_owned(),
        state: a.state.as_str().to_owned(),
        metric: a.metric.clone(),
        at_unix_ms: a.at_unix_ms,
        flapping: a.flapping,
        breach: a.breach.as_ref().map(|b| BreachFacts {
            value: b.value,
            threshold: b.threshold,
            direction: b.direction,
        }),
        asked_about,
    }
}

/// Resolve the names of every node carrying an active alert, in one query rather than one per
/// dependent — a cascade can be thousands of alerts and this runs on an operator's click.
async fn names_of(src: &Sources<'_>, active: &[Alert]) -> BTreeMap<Uuid, String> {
    let mut ids: Vec<Uuid> = active
        .iter()
        .filter_map(|a| Some(uuid_of(a.node()?)))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    // Unrestricted: `active` is the alert set this RCA was already authorized to explain, so the
    // names are for nodes the caller has been shown. Filtering again would blank them mid-report.
    src.nodes
        .node_names(None, &ids)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// Collect the alerts rolled up under `root`, naming the first [`MAX_NAMED_DEPENDENTS`].
///
/// Pure so the truncation behaviour is testable: `total` must keep counting past the cap, because
/// "20 dependents" and "20 of 380 dependents" describe very different outages.
fn roll_up_dependents(
    active: &[Alert],
    root: NodeId,
    names: &BTreeMap<Uuid, String>,
) -> Dependents {
    let mut named: Vec<String> = Vec::new();
    let mut seen: Vec<NodeId> = Vec::new();
    let mut total = 0;
    for a in active.iter().filter(|a| a.root_cause == Some(root)) {
        // Node subjects only: a dependent is a node rolled up under `root`, and only nodes are
        // vertices in the dependency graph that produced `root_cause`.
        let Some(node) = a.node() else { continue };
        // One entry per node, not per check — a node with three failing checks is one dependent.
        if seen.contains(&node) {
            continue;
        }
        seen.push(node);
        total += 1;
        if named.len() < MAX_NAMED_DEPENDENTS {
            let id = uuid_of(node);
            named.push(names.get(&id).cloned().unwrap_or_else(|| id.to_string()));
        }
    }
    named.sort();
    Dependents { named, total }
}

/// Walk the inventory upward from `node`, nearest ancestor first.
///
/// Depth-capped rather than cycle-detected: the inventory is a tree, and the cap keeps a bad edit
/// from turning an operator's click into an unbounded query loop.
async fn upstream_chain(nodes: &NodeRepo, node: &Node) -> Vec<NodeFacts> {
    let mut chain = Vec::new();
    let mut next = node.parent;
    for _ in 0..MAX_UPSTREAM_DEPTH {
        let Some(parent_id) = next else { break };
        let Ok(Some(parent)) = nodes.get_node(uuid_of(parent_id)).await else {
            break;
        };
        next = parent.parent;
        chain.push(NodeFacts::from_node(&parent));
    }
    chain
}

/// Recent audited config changes mentioning this node.
///
/// The audit table records `action` as `"{METHOD} {path}"` with no `node_id` column, so the match
/// is a substring test on the node's uuid. That is done over a bounded, `at DESC`-ordered window
/// (the existing indexed listing) instead of a `LIKE` scan of the whole table — a config change
/// older than [`AUDIT_SCAN_ROWS`] entries is not "recent" in any useful sense anyway.
async fn recent_changes(audit: &AuditRepo, node: Uuid) -> Vec<ChangeFacts> {
    let rows = audit
        .list(&crate::audit::AuditFilter::newest(AUDIT_SCAN_ROWS))
        .await
        .unwrap_or_default();
    filter_changes(&rows, node)
}

/// The pure half of [`recent_changes`], split out so the matching is testable without a database.
fn filter_changes(rows: &[crate::audit::AuditRow], node: Uuid) -> Vec<ChangeFacts> {
    let needle = node.to_string();
    rows.iter()
        .filter(|r| r.action.contains(&needle))
        .take(MAX_RECENT_CHANGES)
        .map(|r| ChangeFacts {
            at: r.at.clone(),
            username: r.username.clone(),
            action: r.action.clone(),
            status: r.status,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::{CheckId, NodeState, Severity};

    fn node_id(n: u128) -> NodeId {
        NodeId::from(Uuid::from_u128(n))
    }

    fn a_node() -> Node {
        let mut node = Node::new(
            node_id(1),
            "core-sw-01",
            "192.168.10.1".parse::<IpAddr>().unwrap(),
        );
        node.vendor = Some("Cisco".to_owned());
        node.model = Some("C9300".to_owned());
        node.pool = Some("branch-osaka".to_owned());
        node
    }

    fn an_alert(node: NodeId, root: Option<NodeId>) -> Alert {
        Alert {
            subject: yagra_alert::Subject::Node(node),
            check: CheckId::from(Uuid::from_u128(99)),
            severity: Severity::Critical,
            state: NodeState::Unreachable,
            at_unix_ms: 1_000,
            root_cause: root,
            flapping: false,
            metric: "__liveness__".to_owned(),
            breach: None,
        }
    }

    // ── Rule 2: credentials cannot reach a prompt ────────────────────────────────────────────

    #[test]
    fn node_facts_carry_the_visible_fields_and_nothing_else() {
        // The canary is planted in a field we DO copy and in one we DON'T. Asserting only the
        // absence would pass even if the whole function were deleted; asserting the presence too
        // proves the test can actually see what crosses the boundary.
        const CANARY: &str = "s3cr3t-community";
        let mut node = a_node();
        node.name = format!("sw-{CANARY}"); // copied — the canary must appear
        node.credential = Some(yagra_common::CredentialId::from(Uuid::from_u128(7))); // never copied
        let facts = NodeFacts::from_node(&node);

        let rendered = format!("{facts:?}");
        assert!(
            rendered.contains(CANARY),
            "the canary must be observable at all: {rendered}"
        );
        assert!(
            !rendered.contains(&Uuid::from_u128(7).to_string()),
            "a credential reference must never cross into the context: {rendered}"
        );
        assert_eq!(facts.address, node.address);
        assert_eq!(facts.pool.as_deref(), Some("branch-osaka"));
    }

    #[test]
    fn tags_are_sorted_and_capped() {
        let mut node = a_node();
        for i in 0..20 {
            node.tags.insert(format!("k{i:02}"), format!("v{i}"));
        }
        let facts = NodeFacts::from_node(&node);
        assert_eq!(facts.tags.len(), MAX_TAGS);
        // Deterministic order: the same node renders identically on every call, so the prompt is
        // cacheable and two RCAs of the same incident are diffable.
        assert_eq!(facts.tags[0].0, "k00");
        assert_eq!(facts.tags[MAX_TAGS - 1].0, format!("k{:02}", MAX_TAGS - 1));
        let again = NodeFacts::from_node(&node);
        assert_eq!(facts.tags, again.tags);
    }

    // ── Rule 3: everything is bounded, and truncation is visible ─────────────────────────────

    #[test]
    fn dependents_are_capped_but_the_total_still_counts() {
        let root = node_id(1);
        let mut names = BTreeMap::new();
        let active: Vec<Alert> = (0..380u128)
            .map(|i| {
                let child = node_id(1000 + i);
                names.insert(uuid_of(child), format!("srv-{i:03}"));
                an_alert(child, Some(root))
            })
            .collect();
        let out = roll_up_dependents(&active, root, &names);
        assert_eq!(out.named.len(), MAX_NAMED_DEPENDENTS);
        // The count survives truncation — "20 of 380" is a different outage from "20".
        assert_eq!(out.total, 380);
    }

    #[test]
    fn a_node_with_several_failing_checks_is_one_dependent() {
        let root = node_id(1);
        let child = node_id(2);
        let mut names = BTreeMap::new();
        names.insert(uuid_of(child), "srv-a".to_owned());
        let mut checks: Vec<Alert> = Vec::new();
        for c in 0..3u128 {
            let mut a = an_alert(child, Some(root));
            a.check = CheckId::from(Uuid::from_u128(c));
            checks.push(a);
        }
        let out = roll_up_dependents(&checks, root, &names);
        assert_eq!(out.total, 1);
        assert_eq!(out.named, vec!["srv-a".to_owned()]);
    }

    #[test]
    fn only_alerts_rolled_up_under_this_root_are_dependents() {
        let root = node_id(1);
        let other = node_id(2);
        let names = BTreeMap::new();
        let active = vec![
            an_alert(node_id(10), Some(root)),
            an_alert(node_id(11), Some(other)), // another incident
            an_alert(node_id(12), None),        // standalone
            an_alert(root, None),               // the cause itself is not its own dependent
        ];
        assert_eq!(roll_up_dependents(&active, root, &names).total, 1);
    }

    #[test]
    fn an_unresolved_name_falls_back_to_the_id_rather_than_being_dropped() {
        // Losing a dependent because its name lookup missed would understate the blast radius.
        let root = node_id(1);
        let orphan = node_id(42);
        let out = roll_up_dependents(&[an_alert(orphan, Some(root))], root, &BTreeMap::new());
        assert_eq!(out.total, 1);
        assert_eq!(out.named, vec![uuid_of(orphan).to_string()]);
    }

    // ── Recent changes ───────────────────────────────────────────────────────────────────────

    fn audit_row(action: &str) -> crate::audit::AuditRow {
        crate::audit::AuditRow {
            id: Uuid::new_v4(),
            at: "2026-07-27T12:00:00Z".to_owned(),
            username: "ops".to_owned(),
            action: action.to_owned(),
            status: 200,
        }
    }

    #[test]
    fn changes_are_matched_by_node_id_and_capped() {
        let node = Uuid::from_u128(1);
        let mut rows = vec![
            audit_row("POST /api/v1/nodes"),
            audit_row(&format!("PUT /api/v1/nodes/{}", Uuid::from_u128(2))),
        ];
        for _ in 0..10 {
            rows.push(audit_row(&format!("PUT /api/v1/nodes/{node}/thresholds")));
        }
        let out = filter_changes(&rows, node);
        assert_eq!(out.len(), MAX_RECENT_CHANGES);
        assert!(out.iter().all(|c| c.action.contains(&node.to_string())));
        // Another node's edit is not this node's context.
        assert!(!out
            .iter()
            .any(|c| c.action.contains(&Uuid::from_u128(2).to_string())));
    }

    #[test]
    fn no_matching_change_is_an_empty_list_not_an_error() {
        let rows = vec![audit_row("POST /api/v1/auth/logout")];
        assert!(filter_changes(&rows, Uuid::from_u128(1)).is_empty());
    }

    // ── Alert shaping ────────────────────────────────────────────────────────────────────────

    #[test]
    fn a_threshold_alert_carries_its_numbers() {
        let mut a = an_alert(node_id(1), None);
        a.metric = "cpu_pct".to_owned();
        a.state = NodeState::Warning;
        a.severity = Severity::Warning;
        a.breach = Some(yagra_alert::Breach {
            value: 91.5,
            threshold: Some(85.0),
            direction: Direction::Above,
        });
        let facts = alert_facts(&a, None);
        assert_eq!(facts.metric, "cpu_pct");
        assert_eq!(facts.severity, "warning");
        assert_eq!(facts.state, "warning");
        let breach = facts.breach.unwrap();
        assert!((breach.value - 91.5).abs() < f64::EPSILON);
        assert_eq!(breach.threshold, Some(85.0));
        assert_eq!(breach.direction, Direction::Above);
        assert!(facts.asked_about.is_none());
    }

    #[test]
    fn a_symptom_click_records_which_node_the_operator_was_looking_at() {
        let facts = alert_facts(&an_alert(node_id(1), None), Some("srv-042".to_owned()));
        assert_eq!(facts.asked_about.as_deref(), Some("srv-042"));
        // Liveness alerts have no numeric breach.
        assert!(facts.breach.is_none());
    }
}
