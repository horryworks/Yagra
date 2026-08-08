// SPDX-License-Identifier: AGPL-3.0-only
//! The variable set a notification template may reference (ADR-039).
//!
//! An operator's subject/body override is rendered against exactly this vocabulary and nothing
//! else. Keeping the list here — rather than beside the renderer, or beside the UI's variable
//! palette — is the whole point: a template is written once and then executed months later during
//! an outage, so a variable that quietly stops being provided is a message that quietly stops
//! saying what it used to.
//!
//! Three properties this module exists to hold:
//!
//! **The context carries no secrets.** SNMP communities, v3 credentials, device logins and API
//! tokens never appear here (ADR-018, `security.md`). A template is operator-authored text that
//! renders into an email or an outbound webhook, which makes the context an export path;
//! `every_exposed_key_is_a_declared_variable` is what keeps a future field from riding along.
//!
//! **An absent fact is *undefined*, never a placeholder.** Optional values are skipped during
//! serialization rather than serialized as `null`, so minijinja sees an undefined name: `{{ value }}`
//! renders empty, `{{ value | default("n/a") }}` works, and `{% if value %}` branches. Serializing
//! `None` would print the literal `none` into somebody's email.
//!
//! **The list is the contract.** [`TEMPLATE_VARIABLES`] drives the API's variable catalogue and
//! therefore the WebUI palette. It is not documentation *about* the context; it is checked against
//! the context by the tests below.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Which point in an alert's life produced this notification.
///
/// Templates branch on it — a resolve usually wants different wording from a fire — so it is a
/// variable rather than three separate template fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotifyEvent {
    /// A new problem alert fired.
    Fire,
    /// A previously-firing alert recovered.
    Resolve,
    /// A still-active alert was rolled up under an upstream root cause, so its incident is closed
    /// even though the node has not recovered.
    Suppress,
}

impl NotifyEvent {
    /// Every event, in lifecycle order.
    pub const ALL: [NotifyEvent; 3] = [
        NotifyEvent::Fire,
        NotifyEvent::Resolve,
        NotifyEvent::Suppress,
    ];

    /// Stable lowercase token — the value of the `event` template variable.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            NotifyEvent::Fire => "fire",
            NotifyEvent::Resolve => "resolve",
            NotifyEvent::Suppress => "suppress",
        }
    }

    /// The inverse of [`Self::as_str`]: an exact token, or `None`.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

impl fmt::Display for NotifyEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One name a notification template may reference.
#[derive(Debug, Clone, Copy, Serialize, utoipa::ToSchema)]
pub struct TemplateVariable {
    /// The name to write between `{{ }}`.
    pub name: &'static str,
    /// What the value means.
    pub description: &'static str,
    /// Whether every alert carries it. A variable that is not always present is *undefined* when
    /// absent: it renders as empty text, and `{{ name | default("…") }}` supplies a fallback.
    pub always_present: bool,
}

/// Every variable a notification template may reference (ADR-039).
///
/// The list an operator sees in the editor comes from here, so this is the one place a variable is
/// named — adding a field to the context without adding it here fails
/// `every_exposed_key_is_a_declared_variable`.
pub const TEMPLATE_VARIABLES: &[TemplateVariable] = &[
    TemplateVariable {
        name: "event",
        description: "Which point in the alert's life this is: fire, resolve, or suppress.",
        always_present: true,
    },
    TemplateVariable {
        name: "subject_kind",
        description: "What the alert is about: `node`, or `pool` when Yagra is reporting that one \
                      of its own poller pools has stopped polling its nodes.",
        always_present: true,
    },
    TemplateVariable {
        name: "subject_name",
        description: "The subject's name — the node's display name, or the poller pool's name. \
                      Prefer this over node_name in a template that should read correctly for \
                      both kinds.",
        always_present: true,
    },
    TemplateVariable {
        name: "node_id",
        description:
            "The node's UUID, which survives a rename and is what an external tool should \
                      correlate on. For a non-node subject this is the subject's own identifier \
                      (`pool:<name>`), never a made-up UUID.",
        always_present: true,
    },
    TemplateVariable {
        name: "node_name",
        description: "The node's display name. Falls back to the UUID if the name cannot be read, \
                      and to the subject's identifier when the alert is not about a node.",
        always_present: true,
    },
    TemplateVariable {
        name: "node_address",
        description: "The node's monitored address.",
        always_present: false,
    },
    TemplateVariable {
        name: "group",
        description: "The name of the inventory folder the node belongs to.",
        always_present: false,
    },
    TemplateVariable {
        name: "profile",
        description: "The name of the monitoring profile bound to the node.",
        always_present: false,
    },
    TemplateVariable {
        name: "check_id",
        description: "The UUID of the check that fired.",
        always_present: true,
    },
    TemplateVariable {
        name: "dedup_key",
        description: "Stable identity for this alert, unchanged between its fire and its resolve. \
                      The same value Yagra sends as the PagerDuty dedup key and the JSM alias.",
        always_present: true,
    },
    TemplateVariable {
        name: "severity",
        description: "How serious the alert is: info, warning, or critical.",
        always_present: true,
    },
    TemplateVariable {
        name: "state",
        description: "The node state that committed: ok, warning, critical, unknown, unreachable, \
                      or maintenance.",
        always_present: true,
    },
    TemplateVariable {
        name: "metric",
        description: "The metric that breached, e.g. icmp_rtt_ms. Absent for an up/down \
                      (liveness) alert, which measures no metric.",
        always_present: false,
    },
    TemplateVariable {
        name: "value",
        description: "The observed sample that committed the transition. A number, so it can be \
                      compared and formatted. Absent unless a threshold was crossed.",
        always_present: false,
    },
    TemplateVariable {
        name: "threshold",
        description: "The bound that was crossed for this severity. Absent unless a threshold was \
                      crossed.",
        always_present: false,
    },
    TemplateVariable {
        name: "direction",
        description: "Which way the metric crossed its bound: above or below. Absent unless a \
                      threshold was crossed.",
        always_present: false,
    },
    TemplateVariable {
        name: "at",
        description: "When the transition committed, as an RFC 3339 timestamp in UTC.",
        always_present: true,
    },
    TemplateVariable {
        name: "at_unix_ms",
        description: "The same instant as milliseconds since the Unix epoch, for arithmetic.",
        always_present: true,
    },
    TemplateVariable {
        name: "flapping",
        description: "True if the node has been changing state rapidly, so this alert is damped.",
        always_present: true,
    },
    TemplateVariable {
        name: "root_cause_id",
        description:
            "The UUID of the upstream node this alert is attributed to. Absent unless the \
                      alert was rolled up.",
        always_present: false,
    },
    TemplateVariable {
        name: "root_cause_name",
        description: "The display name of that upstream node. Absent unless the alert was rolled \
                      up.",
        always_present: false,
    },
];

/// The values a notification template renders against.
///
/// Flat by design: nested objects would let a template reach into a shape that then cannot change,
/// and every name here has to survive being written into somebody's template a year ago.
///
/// `Option` means *undefined*, not `null` — see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AlertFacts {
    /// Lifecycle point (`fire` / `resolve` / `suppress`).
    pub event: NotifyEvent,
    /// What the alert is about: `node`, or `pool` for one of Yagra's own poller pools (ADR-009).
    pub subject_kind: String,
    /// The subject's display name — the node's name, or the pool's. Always present, which is what
    /// makes it the field a template should use when it must read correctly for both kinds.
    pub subject_name: String,
    /// The node's UUID, as text. For a non-node subject this is the subject's own identifier
    /// (`pool:<name>`) rather than a nil or invented UUID — see `notify_facts::context_for`.
    pub node_id: String,
    /// The node's display name, or the UUID when it could not be resolved.
    pub node_name: String,
    /// The node's monitored address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_address: Option<String>,
    /// The inventory folder's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The monitoring profile's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The check's UUID, as text.
    pub check_id: String,
    /// Stable alert identity, shared with the vendor dedup key / alias.
    pub dedup_key: String,
    /// `info` / `warning` / `critical`.
    pub severity: String,
    /// `ok` / `warning` / `critical` / `unknown` / `unreachable` / `maintenance`.
    pub state: String,
    // The liveness sentinel is an internal check name, not a metric; the builder leaves this
    // `None` for an up/down alert rather than printing `__liveness__` into an operator's email.
    // Same call the RCA prompt makes (`rca/prompt.rs`).
    /// The metric that breached; `None` for a liveness alert.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
    /// The observed sample that committed the transition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// The bound crossed for this severity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// `above` / `below`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// RFC 3339 UTC timestamp of the transition.
    pub at: String,
    /// The same instant in milliseconds since the Unix epoch.
    pub at_unix_ms: i64,
    /// Whether the node is flapping.
    pub flapping: bool,
    /// The upstream node this alert is attributed to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_cause_id: Option<String>,
    /// That upstream node's display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_cause_name: Option<String>,
}

/// A representative alert for previewing a template before it is saved.
///
/// Fixed rather than sampled from live data: the preview has to work on a deployment with no
/// alerts, and an operator comparing two edits needs the input to have been the same both times.
/// Every optional variable is populated, so a preview shows the fullest form the template can take.
#[must_use]
pub fn sample_facts(event: NotifyEvent) -> AlertFacts {
    AlertFacts {
        event,
        subject_kind: "node".to_owned(),
        subject_name: "core-sw-01".to_owned(),
        node_id: "6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60".to_owned(),
        node_name: "core-sw-01".to_owned(),
        node_address: Some("192.0.2.11".to_owned()),
        group: Some("Tokyo / Rack 3".to_owned()),
        profile: Some("Cisco switch".to_owned()),
        check_id: "b2a7e4c1-5d38-4f96-8a02-1c7d9e3b6f45".to_owned(),
        dedup_key: "yagra:6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60:\
                    b2a7e4c1-5d38-4f96-8a02-1c7d9e3b6f45:critical"
            .to_owned(),
        severity: "critical".to_owned(),
        state: "critical".to_owned(),
        metric: Some("icmp_rtt_ms".to_owned()),
        value: Some(412.5),
        threshold: Some(200.0),
        direction: Some("above".to_owned()),
        // `at` and `at_unix_ms` are the same instant, and the preview shows both — a mismatch is
        // the kind of thing an operator notices and then distrusts the whole preview over.
        // `the_preview_sample_states_one_instant_two_ways` pins them together.
        //
        // The offset is spelled `+00:00`, not `Z`, because that is what `chrono`'s `to_rfc3339`
        // emits on the real path: a sample that showed `Z` would advertise a format Yagra never
        // sends. `the_preview_sample_is_the_declared_sample` is what caught that.
        at: "2026-08-04T09:41:07+00:00".to_owned(),
        at_unix_ms: 1_785_836_467_000,
        flapping: false,
        root_cause_id: Some("d41f8b06-7c25-4e93-b0a8-5f6c2d19e874".to_owned()),
        root_cause_name: Some("edge-rtr-01".to_owned()),
    }
}

/// The same alert with every optional fact absent — a liveness fire on a node with no group,
/// no profile and no upstream. Used by the tests to prove which variables survive that.
#[must_use]
pub fn minimal_facts(event: NotifyEvent) -> AlertFacts {
    AlertFacts {
        node_address: None,
        group: None,
        profile: None,
        metric: None,
        value: None,
        threshold: None,
        direction: None,
        root_cause_id: None,
        root_cause_name: None,
        ..sample_facts(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn keys(facts: &AlertFacts) -> BTreeSet<String> {
        match serde_json::to_value(facts).expect("facts serialize") {
            serde_json::Value::Object(map) => map.keys().cloned().collect(),
            other => panic!("facts must serialize to an object, got {other:?}"),
        }
    }

    fn declared() -> BTreeSet<String> {
        TEMPLATE_VARIABLES
            .iter()
            .map(|v| v.name.to_owned())
            .collect()
    }

    #[test]
    fn every_event_round_trips_through_its_token_and_through_serde() {
        for e in NotifyEvent::ALL {
            assert_eq!(NotifyEvent::from_token(e.as_str()), Some(e));
            assert_eq!(
                serde_json::to_string(&e).unwrap(),
                format!("\"{}\"", e.as_str())
            );
        }
        assert_eq!(NotifyEvent::from_token("Fire"), None);
    }

    /// The catalogue and the context are one list written twice unless this holds. A variable the
    /// palette offers but the context never provides renders as empty text and looks like a Yagra
    /// bug; a fact the context carries but the catalogue omits is undiscoverable.
    #[test]
    fn every_exposed_key_is_a_declared_variable() {
        assert_eq!(keys(&sample_facts(NotifyEvent::Fire)), declared());
    }

    /// The `always_present` flag is what the editor uses to tell an operator which names need a
    /// `| default(…)`. It is a claim about the data, so it is checked against the data.
    #[test]
    fn always_present_variables_survive_an_alert_with_nothing_optional() {
        let present = keys(&minimal_facts(NotifyEvent::Fire));
        for v in TEMPLATE_VARIABLES {
            assert_eq!(
                present.contains(v.name),
                v.always_present,
                "`{}` is declared always_present={} but is {} on a minimal alert",
                v.name,
                v.always_present,
                if present.contains(v.name) {
                    "present"
                } else {
                    "absent"
                }
            );
        }
    }

    /// An absent fact must reach the engine as *undefined*, not as `null`. `null` renders as the
    /// literal text `none`, which is what an operator would find in their inbox.
    #[test]
    fn an_absent_fact_is_omitted_rather_than_serialized_as_null() {
        let json = serde_json::to_value(minimal_facts(NotifyEvent::Resolve)).unwrap();
        assert!(
            !json.as_object().unwrap().values().any(|v| v.is_null()),
            "no context value may be null: {json}"
        );
        assert!(json.get("value").is_none());
    }

    /// Numbers stay numbers: `{% if value > threshold %}` has to work, and a stringified value
    /// would compare lexicographically without anybody noticing.
    #[test]
    fn numeric_facts_serialize_as_numbers() {
        let json = serde_json::to_value(sample_facts(NotifyEvent::Fire)).unwrap();
        assert!(json["value"].is_number());
        assert!(json["threshold"].is_number());
        assert!(json["at_unix_ms"].is_number());
        assert!(json["flapping"].is_boolean());
    }

    /// Every variable has to be usable inside `{{ }}`, and every description has to say something.
    #[test]
    fn declared_variables_are_usable_names_with_real_descriptions() {
        let mut seen = BTreeSet::new();
        for v in TEMPLATE_VARIABLES {
            assert!(
                seen.insert(v.name),
                "`{}` is declared twice in TEMPLATE_VARIABLES",
                v.name
            );
            assert!(
                !v.name.is_empty()
                    && v.name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    && !v.name.starts_with(|c: char| c.is_ascii_digit()),
                "`{}` is not a plain snake_case identifier",
                v.name
            );
            assert!(
                v.description.trim().len() >= 20,
                "`{}` does not describe what its value means",
                v.name
            );
        }
    }

    /// Keys that must NEVER reach a template (ADR-018 / `security.md`). Same list and same
    /// exact-match discipline as `mcp/dto.rs`'s canary — substring matching would flag `dedup_key`
    /// and get itself deleted. If a future field addition reintroduces one, this fails the build.
    const FORBIDDEN_KEYS: &[&str] = &[
        "credential",
        "community",
        "password",
        "token",
        "pool",
        "auth_key",
        "priv_key",
        "secret",
    ];

    /// The context is an export path — it renders straight into outbound email and webhooks, so a
    /// field that leaks here leaves the deployment. Checked against the serialized context rather
    /// than the catalogue, because the context is what actually ships.
    #[test]
    fn no_credential_material_is_reachable_from_a_template() {
        for event in NotifyEvent::ALL {
            for name in keys(&sample_facts(event)) {
                assert!(
                    !FORBIDDEN_KEYS.contains(&name.as_str()),
                    "forbidden key {name:?} is reachable from a notification template"
                );
            }
        }
    }
}
