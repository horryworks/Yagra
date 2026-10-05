// SPDX-License-Identifier: AGPL-3.0-only
//! The built-in notification a **person** reads: JSM's alert and an email (ADR-194).
//!
//! Webhook and PagerDuty are read by programs, so their built-in body is the alert as JSON
//! (`alerts/notify.rs::json_notification`) and stays byte-identical; since ADR-196 their one-line
//! summary is this module's [`subject`]. JSM shows its `description` as plain
//! text and an email body is plain text, so for those two the JSON reached the reader verbatim.
//! This module writes what they get instead: a title naming the device, a body of one fact per
//! line, and JSM's `details` (its "extra properties" table).
//!
//! **Plain `format!`, never a template** (ADR-039 decision 3): this is also what a broken template
//! falls back to, and a fallback that runs through the machinery that just failed is not one.
//!
//! Everything here is pure. The facts are resolved by the caller; when they could not be, the
//! caller passes the facts `context_for` builds from the alert alone, which name the node by id.

use yagra_alert::{Alert, Subject};
use yagra_common::{AlertFacts, NotifyEvent};

/// The longest value put into one JSM `details` entry. JSM caps the whole map at 8,000 characters;
/// sixteen keys at this length, with their names, stay inside it.
pub(crate) const DETAIL_VALUE_MAX_CHARS: usize = 450;

/// Prefix of the test notification's text body (ADR-192 decision 1 / ADR-194 decision 5).
pub(crate) const TEST_BODY_LINE: &str = "This is a test notification from Yagra.";

/// `core-sw-01 (192.0.2.10)`, or just the name when the address is unknown.
fn who(facts: &AlertFacts) -> String {
    match facts.node_address.as_deref() {
        Some(addr) if !addr.is_empty() && addr != facts.node_name => {
            format!("{} ({addr})", facts.node_name)
        }
        _ => facts.node_name.clone(),
    }
}

/// A number as a person writes it: `97`, not `97.0`; `0.25`, not `0.25000000000000006`.
pub(crate) fn fmt_num(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.0}")
    } else {
        let s = format!("{v:.3}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        s.to_owned()
    }
}

/// The port as a person finds it: its name with the index beside it, or the index alone when the
/// name is not known (ADR-196 decision 6).
fn port(facts: &AlertFacts) -> Option<String> {
    let i = facts.ifindex?;
    Some(match facts.if_name.as_deref() {
        Some(name) => format!("{name} (ifIndex {i})"),
        None => format!("ifIndex {i}"),
    })
}

/// `CPU usage (5 min)`, `Inbound utilization on Gi0/3`, `Memory pool usage [I/O]` — the alert's
/// name (ADR-196) and where on the device it is.
fn title_where(title: &str, facts: &AlertFacts) -> String {
    let mut out = title.to_owned();
    match (facts.if_name.as_deref(), facts.ifindex) {
        (Some(name), Some(_)) => out.push_str(&format!(" on {name}")),
        (None, Some(i)) => out.push_str(&format!(" on ifIndex {i}")),
        (_, None) => {}
    }
    if let Some(row) = facts.row_name.as_deref() {
        out.push_str(&format!(" [{row}]"));
    }
    out
}

/// `: SNMP not responding`, `: Inbound utilization on Gi0/3`, or nothing for an alert with no name.
///
/// The numbers stay in the body and the details. Here they would not survive the editor's draft:
/// a template prints `97.0` where a person writes `97`, and the draft has to render this exactly.
fn breach_clause(facts: &AlertFacts) -> String {
    facts
        .title
        .as_deref()
        .map(|t| format!(": {}", title_where(t, facts)))
        .unwrap_or_default()
}

/// The title. The pool and Meraki organization sentences are the JSON built-in's, with the
/// subject's name in place of its id.
#[must_use]
pub(crate) fn subject(alert: &Alert, facts: &AlertFacts) -> String {
    let name = &facts.subject_name;
    match (&alert.subject, facts.event) {
        (Subject::Node(_), NotifyEvent::Fire) => {
            format!("{} is {}{}", who(facts), facts.state, breach_clause(facts))
        }
        (Subject::Node(_), NotifyEvent::Resolve) => format!("resolved: {} recovered", who(facts)),
        (Subject::Node(_), NotifyEvent::Suppress) => match facts.root_cause_name.as_deref() {
            Some(root) => format!("rolled up: {} suppressed under upstream {root}", who(facts)),
            None => format!("rolled up: {} suppressed under upstream", who(facts)),
        },
        (Subject::Pool(_), NotifyEvent::Fire) => {
            format!("poller pool \"{name}\" has no live poller — its nodes are not being monitored")
        }
        (Subject::Pool(_), NotifyEvent::Resolve) => {
            format!("resolved: poller pool \"{name}\" has a live poller again")
        }
        (Subject::Pool(_), NotifyEvent::Suppress) => {
            format!("rolled up: poller pool \"{name}\" suppressed")
        }
        (Subject::MerakiOrg(_), NotifyEvent::Fire) => format!(
            "Meraki organization {name}: the Dashboard API is not answering — its devices' states \
             are the last ones collected"
        ),
        (Subject::MerakiOrg(_), NotifyEvent::Resolve) => {
            format!("resolved: Meraki organization {name} is being collected again")
        }
        (Subject::MerakiOrg(_), NotifyEvent::Suppress) => {
            format!("rolled up: Meraki organization {name} suppressed")
        }
    }
}

/// The body: the title, a blank line, then one fact per line. A fact the alert does not carry is
/// left out rather than printed empty.
#[must_use]
pub(crate) fn body(alert: &Alert, facts: &AlertFacts) -> String {
    let mut lines: Vec<(&str, String)> = Vec::new();
    match &alert.subject {
        Subject::Node(_) => lines.push(("Node", who(facts))),
        Subject::Pool(_) => lines.push(("Poller pool", facts.subject_name.clone())),
        Subject::MerakiOrg(_) => lines.push(("Meraki organization", facts.subject_name.clone())),
    }
    if let Some(g) = &facts.group {
        lines.push(("Folder", g.clone()));
    }
    if let Some(p) = &facts.profile {
        lines.push(("Profile", p.clone()));
    }
    if let Some(t) = &facts.title {
        lines.push(("Alert", t.clone()));
    }
    // The raw metric stays: it is what a rule names and what a mail filter written before ADR-196
    // matched in the title.
    if let Some(m) = &facts.metric {
        let mut v = m.clone();
        if let Some(value) = facts.value {
            v.push_str(&format!(" = {}", fmt_num(value)));
        }
        if let (Some(dir), Some(t)) = (facts.direction.as_deref(), facts.threshold) {
            v.push_str(&format!(" (threshold: {dir} {})", fmt_num(t)));
        }
        lines.push(("Metric", v));
    }
    if let Some(p) = port(facts) {
        lines.push(("Port", p));
    }
    if let Some(r) = &facts.row_name {
        lines.push(("Row", r.clone()));
    }
    lines.push(("Severity", facts.severity.clone()));
    lines.push(("State", facts.state.clone()));
    lines.push(("Since", facts.at.clone()));
    if !facts.tags.is_empty() {
        lines.push(("Tags", facts.tags.join(", ")));
    }
    if let Some(root) = &facts.root_cause_name {
        lines.push(("Rolled up under", root.clone()));
    }
    if facts.flapping {
        lines.push(("Flapping", "yes".to_owned()));
    }
    lines.push(("Alert key", facts.dedup_key.clone()));
    if matches!(alert.subject, Subject::Node(_)) {
        lines.push(("Node ID", facts.node_id.clone()));
    }

    let mut out = subject(alert, facts);
    out.push_str("\n\n");
    for (label, value) in lines {
        out.push_str(&format!("{:<10} {value}\n", format!("{label}:")));
    }
    out
}

/// JSM's `details` ("extra properties"), in a fixed order. Keys with no value are left out, and
/// each value is cut at [`DETAIL_VALUE_MAX_CHARS`].
#[must_use]
pub(crate) fn details(facts: &AlertFacts) -> Vec<(String, String)> {
    let num = |v: Option<f64>| v.map(fmt_num);
    let pairs: [(&str, Option<String>); 16] = [
        ("node", Some(facts.subject_name.clone())),
        ("address", facts.node_address.clone()),
        ("folder", facts.group.clone()),
        ("profile", facts.profile.clone()),
        ("alert", facts.title.clone()),
        ("metric", facts.metric.clone()),
        ("value", num(facts.value)),
        ("threshold", num(facts.threshold)),
        ("direction", facts.direction.clone()),
        ("port", facts.if_name.clone()),
        ("ifindex", facts.ifindex.map(|i| i.to_string())),
        ("row", facts.row_name.clone()),
        ("severity", Some(facts.severity.clone())),
        ("state", Some(facts.state.clone())),
        ("node_id", Some(facts.node_id.clone())),
        ("subject_kind", Some(facts.subject_kind.clone())),
    ];
    pairs
        .into_iter()
        .filter_map(|(k, v)| {
            let v = v.filter(|v| !v.is_empty())?;
            Some((
                k.to_owned(),
                v.chars().take(DETAIL_VALUE_MAX_CHARS).collect(),
            ))
        })
        .collect()
}

/// The draft the template editor opens a JSM or email channel on: [`subject`] for a node alert,
/// written as a template (ADR-194 decision 6). Rendering it gives exactly what [`subject`] gives —
/// `alerts/notify.rs::every_builtin_subject_template_renders_the_builtin_subject` pins the two
/// together.
#[must_use]
pub(crate) const fn node_subject_template(event: NotifyEvent) -> &'static str {
    match event {
        NotifyEvent::Fire => concat!(
            "{{ node_name }}{% if node_address and node_address != node_name %} ",
            "({{ node_address }}){% endif %} is {{ state }}",
            "{% if title %}: {{ title }}",
            "{% if if_name and ifindex is defined %} on {{ if_name }}",
            "{% elif ifindex is defined %} on ifIndex {{ ifindex }}{% endif %}",
            "{% if row_name %} [{{ row_name }}]{% endif %}",
            "{% endif %}"
        ),
        NotifyEvent::Resolve => concat!(
            "resolved: {{ node_name }}{% if node_address and node_address != node_name %} ",
            "({{ node_address }}){% endif %} recovered"
        ),
        NotifyEvent::Suppress => concat!(
            "rolled up: {{ node_name }}{% if node_address and node_address != node_name %} ",
            "({{ node_address }}){% endif %} suppressed under upstream",
            "{% if root_cause_name %} {{ root_cause_name }}{% endif %}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify_facts::{context_for, preview_sample};
    use std::collections::HashMap;
    use yagra_common::PreviewSample;

    #[test]
    fn numbers_read_the_way_a_person_writes_them() {
        assert_eq!(fmt_num(97.0), "97");
        assert_eq!(fmt_num(0.25), "0.25");
        assert_eq!(fmt_num(-3.5), "-3.5");
        assert_eq!(fmt_num(1.0 / 3.0), "0.333");
        assert_eq!(fmt_num(f64::NAN), "NaN");
    }

    #[test]
    fn a_threshold_title_names_the_device_and_the_metric() {
        let (alert, resolved) = preview_sample(PreviewSample::Threshold);
        let facts = context_for(&alert, NotifyEvent::Fire, &resolved);
        let s = subject(&alert, &facts);
        assert!(s.starts_with(&format!("{} (", facts.node_name)), "{s}");
        // The alert's name and the port's, never the raw metric (ADR-196).
        assert_eq!(
            s,
            "core-sw-01 (192.0.2.11) is critical: Inbound utilization on ifIndex 7"
        );
        assert!(!s.contains(facts.metric.as_deref().unwrap()), "{s}");
        let mut named = facts.clone();
        named.if_name = Some("Gi0/7".to_owned());
        assert!(subject(&alert, &named).ends_with("Inbound utilization on Gi0/7"));
        assert!(body(&alert, &named).contains("Port:      Gi0/7 (ifIndex 7)"));
        assert!(
            !s.contains(&facts.node_id),
            "the title names, never the uuid: {s}"
        );
    }

    #[test]
    fn an_unresolved_node_is_named_by_its_id_and_nothing_is_invented() {
        let (alert, _) = preview_sample(PreviewSample::Liveness);
        let facts = context_for(&alert, NotifyEvent::Fire, &HashMap::new());
        let s = subject(&alert, &facts);
        assert_eq!(
            s,
            format!("{} is {}: Node not responding", facts.node_id, facts.state)
        );
        let b = body(&alert, &facts);
        assert!(!b.contains("Folder:") && !b.contains("Profile:"), "{b}");
    }

    #[test]
    fn the_body_is_text_with_one_fact_per_line_and_never_json() {
        for sample in PreviewSample::ALL {
            let (alert, resolved) = preview_sample(sample);
            for event in NotifyEvent::ALL {
                let facts = context_for(&alert, event, &resolved);
                let b = body(&alert, &facts);
                assert!(
                    serde_json::from_str::<serde_json::Value>(&b).is_err(),
                    "{b}"
                );
                assert!(b.starts_with(&subject(&alert, &facts)), "{b}");
                assert!(
                    b.contains(&format!("Node:      {}", facts.node_name)),
                    "{b}"
                );
                assert!(b.contains(&facts.dedup_key), "{b}");
                assert!(!b.contains(": \n"), "no empty fact: {b}");
            }
        }
    }

    #[test]
    fn details_carry_the_facts_and_leave_out_what_is_absent() {
        let (alert, resolved) = preview_sample(PreviewSample::Threshold);
        let facts = context_for(&alert, NotifyEvent::Fire, &resolved);
        let d: HashMap<_, _> = details(&facts).into_iter().collect();
        assert_eq!(d["node"], facts.node_name);
        assert_eq!(d["address"], facts.node_address.clone().unwrap());
        assert_eq!(d["metric"], facts.metric.clone().unwrap());
        assert_eq!(d["subject_kind"], "node");

        let (alert, _) = preview_sample(PreviewSample::Liveness);
        let bare = context_for(&alert, NotifyEvent::Fire, &HashMap::new());
        let keys: Vec<String> = details(&bare).into_iter().map(|(k, _)| k).collect();
        for absent in ["address", "folder", "metric", "value", "threshold"] {
            assert!(!keys.iter().any(|k| k == absent), "{absent} in {keys:?}");
        }
    }

    #[test]
    fn a_detail_value_is_cut_to_its_cap() {
        let (alert, resolved) = preview_sample(PreviewSample::Threshold);
        let mut facts = context_for(&alert, NotifyEvent::Fire, &resolved);
        facts.group = Some("あ".repeat(DETAIL_VALUE_MAX_CHARS + 10));
        let d: HashMap<_, _> = details(&facts).into_iter().collect();
        assert_eq!(d["folder"].chars().count(), DETAIL_VALUE_MAX_CHARS);
    }
}
