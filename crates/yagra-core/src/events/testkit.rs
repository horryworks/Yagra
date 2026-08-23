// SPDX-License-Identifier: AGPL-3.0-only
//! Fixtures shared by more than one of `events/`'s test modules (ADR-095).
//!
//! Only what genuinely crosses a file lives here. A helper used by one module stays in that module,
//! because a testkit that collects everything becomes a second place to look for every fixture —
//! `syslog_msg` and `trap_msg` are here because the planner tests and `persist_record` both build
//! messages, `stored_rule` because the rule tests and the planner tests both need one, and
//! `persist_record` because `PersistRecord::is_alert_linked` and the persist writer both classify
//! the same record.
//!
//! Declared `#[cfg(test)] mod testkit;` in [`super`], which is how [`crate::module_source`]'s
//! exclusion derives it — a fixture file inside the directory a guard scans would otherwise match
//! that guard's own needles (ADR-086 hit this within minutes of splitting `mcp/tools.rs`).

use chrono::Utc;
use uuid::Uuid;
use yagra_bus::{EventKind, EventMsg};

// The vocabulary lives in the parent, which a child can see without any widening — see
// `super`'s doc for why that is what decides where a thing goes here.
use super::*;

pub(super) fn stored_rule(pattern: &str, severity: &str) -> StoredEventRule {
    StoredEventRule {
        id: Uuid::new_v4(),
        name: "test rule".into(),
        enabled: true,
        source_kind: None,
        source_id: None,
        node_id: None,
        match_kind: EventMatchKind::Substring,
        pattern: pattern.into(),
        clear_pattern: None,
        // Still takes the token, so the callers below read as the stored rows they stand for.
        severity: parse_severity(severity),
        ttl_secs: 1800,
        min_count: 1,
        window_secs: 60,
        created_at: Utc::now(),
    }
}

pub(super) fn persist_record(action: EventAction) -> PersistRecord {
    let msg = syslog_msg("some event body");
    PersistRecord {
        signature: signature_of(&msg),
        msg,
        node_id: Some(Uuid::new_v4()),
        source_id: None,
        matched_rule_id: (action != EventAction::None).then(Uuid::new_v4),
        action,
    }
}

pub(super) fn syslog_msg(message: &str) -> EventMsg {
    EventMsg {
        event_id: Uuid::new_v4(),
        kind: EventKind::Syslog,
        at_unix_ms: 1_000,
        source_ip: Some("10.0.0.1".parse().unwrap()),
        pool: None,
        message: message.into(),
        facility: None,
        syslog_severity: None,
        hostname: None,
        app_name: None,
        trap_oid: None,
        varbinds: Vec::new(),
        truncated: false,
        raw: None,
        src_port: None,
    }
}

/// Mirrors what the poller publishes for an SNMP trap: `message` begins with the raw
/// identity OID (`render_message`), and `trap_oid` carries that identity for name resolution.
pub(super) fn trap_msg(trap_oid: &str) -> EventMsg {
    EventMsg {
        event_id: Uuid::new_v4(),
        kind: EventKind::Trap,
        at_unix_ms: 1_000,
        source_ip: Some("10.0.0.1".parse().unwrap()),
        pool: None,
        message: format!("{trap_oid} 1.3.6.1.2.1.2.2.1.1.4=4;"),
        facility: None,
        syslog_severity: None,
        hostname: None,
        app_name: None,
        trap_oid: Some(trap_oid.into()),
        varbinds: vec![("1.3.6.1.2.1.2.2.1.1.4".into(), "4".into())],
        truncated: false,
        raw: None,
        src_port: None,
    }
}
