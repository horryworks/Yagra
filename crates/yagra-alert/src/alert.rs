// SPDX-License-Identifier: AGPL-3.0-only
//! Alert identity: dedup and grouping keys.
//!
//! Dedup collapses duplicate alerts for the same `(subject, check, severity)`; grouping rolls
//! related alerts up under their root-cause node so a parent outage shows as one incident
//! plus its children, not N pages (ADR-015). The alert *lifecycle* is then forwarded to an
//! external tool — Yagra owns the quality, not the escalation.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use yagra_common::{CheckId, Direction, NodeId, NodeState, Severity};

/// Prefix distinguishing a pool subject from a node subject in the flat string form.
///
/// A `NodeId` renders as a bare hyphenated UUID, which contains no `:`, so the two forms
/// cannot collide in either direction.
const POOL_PREFIX: &str = "pool:";

/// Namespace for the UUIDv5 identities non-node subjects are stored under.
///
/// A fixed literal rather than a derived value so it is `const` and can never move: every row in
/// `alert_history` and `alert_acks` written for a pool is keyed by a name hashed under this, so
/// changing it would orphan every stored pool alert and every open acknowledgement. The bytes spell
/// `YAGRA_SUBJECT_ID`, which is only a mnemonic — nothing reads them back.
const SUBJECT_NAMESPACE: uuid::Uuid =
    uuid::Uuid::from_u128(0x5941_4752_415f_5355_424a_4543_545f_4944);

/// Which kind of thing an alert is about: a monitored `node`, or a poller `pool` when Yagra is
/// reporting on its own polling coverage.
//
// This doc comment is published verbatim to API clients (ADR-035), so the internal note goes here:
// the value is both a database column and a JSON field, produced by two different mechanisms, and
// `every_subject_kind_round_trips_through_its_token_and_through_serde` is what pins them together
// (`testing.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    /// A monitored node.
    Node,
    /// A poller pool.
    Pool,
}

impl SubjectKind {
    /// Every kind, for exhaustive iteration in tests and UI enumerations.
    pub const ALL: [Self; 2] = [Self::Node, Self::Pool];

    /// The stored/serialized token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Pool => "pool",
        }
    }

    /// Parse a stored token. `None` for anything a newer core may have written.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == token)
    }
}

/// What an alert is about.
///
/// Almost every alert is about a monitored node. Yagra also alerts on its **own** monitoring
/// coverage — a poller pool that has nodes but no live poller means those nodes are not being
/// polled at all — and that condition has no node to hang off.
//
// Why a sum type rather than a synthetic node id: `api/scope.rs`'s `allows_node` resolves a
// node's folder group through `AlertManager::node_folder_group`, which answers `None` for an id
// the config snapshot has never seen, and `ScopeSet::allows` is fail-closed. A synthetic id would
// therefore be refused for every group-scoped operator — hiding the alert from precisely the
// person responsible for the site that went dark — while remaining visible to unrestricted ones.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    /// A monitored node.
    Node(NodeId),
    /// A poller pool, named. Used by coverage alerts about Yagra's own polling.
    Pool(String),
}

impl Subject {
    /// The node this is about, or `None` for a non-node subject.
    #[must_use]
    pub const fn node(&self) -> Option<NodeId> {
        match self {
            Self::Node(n) => Some(*n),
            Self::Pool(_) => None,
        }
    }

    /// Whether this subject is exactly `node`.
    #[must_use]
    pub fn is_node(&self, node: NodeId) -> bool {
        matches!(self, Self::Node(n) if *n == node)
    }

    /// The pool this is about, or `None` for a node subject.
    #[must_use]
    pub fn pool(&self) -> Option<&str> {
        match self {
            Self::Pool(p) => Some(p.as_str()),
            Self::Node(_) => None,
        }
    }

    /// Which kind of subject this is.
    #[must_use]
    pub const fn kind(&self) -> SubjectKind {
        match self {
            Self::Node(_) => SubjectKind::Node,
            Self::Pool(_) => SubjectKind::Pool,
        }
    }

    /// The subject's **name**, for a subject that is named rather than identified. `None` for a
    /// node, whose name lives in the inventory and is resolved from its id.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Node(_) => None,
            Self::Pool(p) => Some(p.as_str()),
        }
    }

    /// The UUID this subject is **stored** under (`alert_history.node`, `alert_acks.node`).
    ///
    /// For a node it is the node id itself, so every existing row and every open acknowledgement
    /// keeps its identity byte-for-byte. For anything else it is a stable v5 hash of the flat form
    /// under [`SUBJECT_NAMESPACE`], which buys three things at once:
    ///
    ///  - the columns stay `NOT NULL`, so an **N-1 core** rolled back onto this data still decodes
    ///    every row into its bare `Uuid` and renders the History page instead of failing it (a
    ///    nullable column would 500 the whole page for one row — ADR-017);
    ///  - `alert_acks`' primary key `(node, check_id, severity)` needs no rewrite, so this stays an
    ///    additive migration;
    ///  - the ack join keeps working for pool alerts without a second key shape.
    ///
    /// It is an **identity token, not a node reference**: `subject_kind` is what says whether the
    /// value names a node, and every read that means "a node" filters on that column rather than
    /// trusting this one to resolve.
    #[must_use]
    pub fn storage_id(&self) -> uuid::Uuid {
        match self {
            Self::Node(n) => n.as_uuid(),
            // Hashed from the flat form (`pool:<name>`), which already namespaces the pool's own
            // free-text name — see `Display`. Spelled per variant rather than through a wildcard so
            // a third subject kind has to decide this rather than inheriting it.
            Self::Pool(_) => uuid::Uuid::new_v5(&SUBJECT_NAMESPACE, self.to_string().as_bytes()),
        }
    }

    /// Rebuild a subject from its stored columns.
    ///
    /// `None` when the row cannot be read as a subject — an unknown `kind` (a newer core wrote it)
    /// or a `pool` row with no name. Callers degrade the row rather than failing the page, the same
    /// way an unrecognised severity token does.
    #[must_use]
    pub fn from_storage(kind: SubjectKind, id: uuid::Uuid, name: Option<&str>) -> Option<Self> {
        match kind {
            SubjectKind::Node => Some(Self::Node(NodeId::from(id))),
            SubjectKind::Pool => name
                .filter(|n| !n.is_empty())
                .map(|n| Self::Pool(n.to_owned())),
        }
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // A bare UUID, byte-identical to what `NodeId` renders — this is what keeps the
            // wire form, the dedup string and every existing check id unchanged.
            Self::Node(n) => write!(f, "{n}"),
            Self::Pool(p) => write!(f, "{POOL_PREFIX}{p}"),
        }
    }
}

/// A subject string that is neither a UUID nor a `pool:`-prefixed name.
#[derive(Debug, thiserror::Error)]
#[error("not a valid alert subject: {0}")]
pub struct ParseSubjectError(String);

impl FromStr for Subject {
    type Err = ParseSubjectError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(pool) = s.strip_prefix(POOL_PREFIX) {
            return if pool.is_empty() {
                Err(ParseSubjectError(s.to_owned()))
            } else {
                Ok(Self::Pool(pool.to_owned()))
            };
        }
        uuid::Uuid::parse_str(s)
            .map(|u| Self::Node(NodeId::from(u)))
            .map_err(|_| ParseSubjectError(s.to_owned()))
    }
}

// Serialized as a flat string so a node subject is byte-identical to the `NodeId` it replaced.
// A client that only ever saw node alerts therefore reads exactly what it read before; the
// `subject_kind` / `subject_name` fields beside it are what let a client tell the two apart.
impl Serialize for Subject {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Subject {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// Dedup key: two alerts with the same key are the same alert.
//
// No longer `Copy`: `Subject::Pool` owns its name. `Hash + Eq` are what `Dispatcher`'s
// `HashSet<DedupKey>` needs, and both survive.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DedupKey {
    /// What the alert is about.
    pub subject: Subject,
    /// The check that fired.
    pub check: CheckId,
    /// Severity of the alert.
    pub severity: Severity,
}

/// Numeric breach detail for a threshold alert (absent for a liveness up/down alert).
/// Carried for the history log + notification payload — not part of alert identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Breach {
    /// Observed sample value that committed the transition.
    pub value: f64,
    /// The bound crossed for the committed severity, if the rule defines one at that level.
    pub threshold: Option<f64>,
    /// Which way the metric crossed its bound.
    pub direction: Direction,
}

/// A single alert produced by the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Alert {
    /// What the alert is about: a node's UUID, or `pool:<name>` for a poller-pool alert.
    //
    // The wire name stays `node` and a node subject stays a bare UUID string, so every alert a
    // client saw before this field became a sum type is byte-identical. What changed is that the
    // value is no longer *always* a UUID — hence the documented type is now `String`, and every
    // response carrying an alert also carries a `subject_kind` (and a `subject_name` for a named
    // subject) beside it so a client branches on the kind instead of guessing from the shape.
    #[serde(rename = "node")]
    #[schema(value_type = String)]
    pub subject: Subject,
    /// The check that produced it.
    pub check: CheckId,
    /// Severity (derived from the committed state).
    pub severity: Severity,
    /// The committed state that triggered the alert.
    pub state: NodeState,
    /// When it fired (Unix ms, UTC).
    pub at_unix_ms: i64,
    /// Root-cause node, if this alert was attributed upstream by dependency analysis.
    pub root_cause: Option<NodeId>,
    /// Whether the underlying check is currently flapping.
    pub flapping: bool,
    /// Metric the check measured (e.g. `"icmp_rtt_ms"`; the liveness sentinel for up/down).
    /// Carried for the history log + notification payload so a human can read *what* fired —
    /// not part of alert identity (dedup/grouping ignore it).
    pub metric: String,
    /// Numeric breach detail for a threshold alert; `None` for a liveness alert.
    pub breach: Option<Breach>,
}

impl Alert {
    /// The dedup key for this alert.
    #[must_use]
    pub fn dedup_key(&self) -> DedupKey {
        DedupKey {
            subject: self.subject.clone(),
            check: self.check,
            severity: self.severity,
        }
    }

    /// The node this alert is about, or `None` if its subject is not a node.
    #[must_use]
    pub const fn node(&self) -> Option<NodeId> {
        self.subject.node()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(node: NodeId, root: Option<NodeId>) -> Alert {
        Alert {
            subject: Subject::Node(node),
            check: CheckId::from(uuid_nil()),
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 0,
            root_cause: root,
            flapping: false,
            metric: "__liveness__".to_string(),
            breach: None,
        }
    }

    fn uuid_nil() -> uuid::Uuid {
        uuid::Uuid::nil()
    }

    #[test]
    fn dedup_key_ignores_timestamp_and_flapping() {
        let n = NodeId::new();
        let mut a = alert(n, None);
        let mut b = alert(n, None);
        a.at_unix_ms = 100;
        b.at_unix_ms = 999;
        b.flapping = true;
        assert_eq!(a.dedup_key(), b.dedup_key());
    }

    #[test]
    fn a_node_subject_is_wire_identical_to_the_node_id_it_replaced() {
        // Load-bearing: this is what lets `Alert` keep documenting the field as a `NodeId`, and
        // what stops every persisted/serialized node alert from changing shape. Do not "tidy"
        // `Subject`'s Serialize into a tagged enum without widening the API contract first.
        let n = NodeId::new();
        let subject = serde_json::to_string(&Subject::Node(n)).unwrap();
        assert_eq!(subject, serde_json::to_string(&n).unwrap());

        let a = alert(n, None);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(v["node"], serde_json::Value::String(n.to_string()));
        assert!(v.get("subject").is_none(), "the wire field is still `node`");
    }

    #[test]
    fn an_alert_serialized_before_the_subject_split_still_deserializes() {
        // N-1: a node alert written by an older core (or sitting in a queue) must still load.
        let n = NodeId::new();
        let json = format!(
            r#"{{"node":"{n}","check":"{nil}","severity":"critical","state":"critical",
                 "at_unix_ms":7,"root_cause":null,"flapping":false,"metric":"__liveness__",
                 "breach":null}}"#,
            nil = uuid_nil()
        );
        let a: Alert = serde_json::from_str(&json).unwrap();
        assert_eq!(a.subject, Subject::Node(n));
        assert_eq!(a.node(), Some(n));
        assert_eq!(a.at_unix_ms, 7);
    }

    #[test]
    fn every_subject_round_trips_through_its_string_form() {
        for s in [
            Subject::Node(NodeId::new()),
            Subject::Pool("default".to_owned()),
            // Operator-authored, so it may carry anything a pool name may carry.
            Subject::Pool("tokyo-dc 2".to_owned()),
            Subject::Pool("pool:nested".to_owned()),
        ] {
            assert_eq!(s.to_string().parse::<Subject>().unwrap(), s, "{s}");
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(serde_json::from_str::<Subject>(&json).unwrap(), s, "{s}");
        }
    }

    #[test]
    fn a_pool_named_like_a_uuid_is_not_a_node() {
        // The prefix is what separates the two namespaces; a pool name is free text and could
        // otherwise impersonate a node id.
        let looks_like_a_node = NodeId::new().to_string();
        let s = Subject::Pool(looks_like_a_node.clone());
        assert_eq!(s.to_string().parse::<Subject>().unwrap(), s);
        assert_eq!(s.node(), None);
        // …and a bare uuid never reads back as a pool.
        assert!(matches!(
            looks_like_a_node.parse::<Subject>().unwrap(),
            Subject::Node(_)
        ));
    }

    #[test]
    fn a_subject_string_that_is_neither_is_refused() {
        for bad in ["", "not-a-uuid", POOL_PREFIX, "pool"] {
            assert!(bad.parse::<Subject>().is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn a_node_subject_is_stored_under_its_own_id() {
        // Load-bearing for the migration: `alert_history.node` and `alert_acks.node` keep exactly
        // the values they already hold for every node alert, so no row is re-keyed and no open
        // acknowledgement is orphaned.
        let n = NodeId::new();
        assert_eq!(Subject::Node(n).storage_id(), n.as_uuid());
    }

    #[test]
    fn a_pool_storage_id_is_stable_and_distinct_per_pool() {
        // Stability is what makes a fire and its later resolve/ack agree on one row. Recomputed
        // rather than asserted against a literal so the test states the property, not the digest —
        // but the namespace it hashes under must never change, which is why that constant is fixed.
        let a = Subject::Pool("tokyo".to_owned()).storage_id();
        assert_eq!(a, Subject::Pool("tokyo".to_owned()).storage_id());
        assert_ne!(a, Subject::Pool("osaka".to_owned()).storage_id());
        // …and a pool named like the flat form of another subject does not borrow its id.
        assert_ne!(a, Subject::Pool("pool:tokyo".to_owned()).storage_id());
    }

    #[test]
    fn a_pool_id_cannot_impersonate_the_node_whose_name_it_carries() {
        // A pool name is operator-authored free text, so it can be spelled as a node's UUID. The
        // id it is stored under must still not be that node's, or a pool alert would decorate
        // itself with that node's acknowledgement.
        let n = NodeId::new();
        let impostor = Subject::Pool(n.to_string());
        assert_ne!(impostor.storage_id(), n.as_uuid());
    }

    #[test]
    fn a_subject_round_trips_through_its_stored_columns() {
        for s in [
            Subject::Node(NodeId::new()),
            Subject::Pool("tokyo".to_owned()),
            Subject::Pool("dc 2/edge".to_owned()),
        ] {
            let restored = Subject::from_storage(s.kind(), s.storage_id(), s.name());
            assert_eq!(restored.as_ref(), Some(&s), "{s}");
        }
        // A pool row with no name cannot be read back — it degrades rather than inventing one.
        assert_eq!(
            Subject::from_storage(SubjectKind::Pool, uuid::Uuid::nil(), None),
            None
        );
        assert_eq!(
            Subject::from_storage(SubjectKind::Pool, uuid::Uuid::nil(), Some("")),
            None
        );
    }

    #[test]
    fn every_subject_kind_round_trips_through_its_token_and_through_serde() {
        // The token is a DB column and the serde tag is a JSON field; they are produced by two
        // different mechanisms and nothing else makes them agree (`testing.md`).
        for kind in SubjectKind::ALL {
            assert_eq!(SubjectKind::from_token(kind.as_str()), Some(kind));
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
        }
        assert_eq!(SubjectKind::from_token("cluster"), None);
    }

    #[test]
    fn kind_and_name_agree_with_the_variant() {
        let n = NodeId::new();
        assert_eq!(Subject::Node(n).kind(), SubjectKind::Node);
        assert_eq!(Subject::Node(n).name(), None);
        assert_eq!(Subject::Pool("x".to_owned()).kind(), SubjectKind::Pool);
        assert_eq!(Subject::Pool("x".to_owned()).name(), Some("x"));
    }

    #[test]
    fn node_and_pool_subjects_never_share_a_dedup_key() {
        let n = NodeId::new();
        let mut a = alert(n, None);
        let mut b = alert(n, None);
        b.subject = Subject::Pool(n.to_string());
        assert_ne!(a.dedup_key(), b.dedup_key());
        a.subject = Subject::Pool("x".to_owned());
        b.subject = Subject::Pool("x".to_owned());
        assert_eq!(a.dedup_key(), b.dedup_key());
    }
}
