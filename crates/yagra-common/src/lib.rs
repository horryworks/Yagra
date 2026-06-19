//! yagra-common — shared types and models across Yagra components.
//!
//! Node/device-class, metric, alert-state, and severity types that more than one
//! component needs to agree on live here. Keep protocol- or store-specific types
//! out of this crate; this is the cross-cutting vocabulary only.
//!
//! Design anchors: stable IDs as the only series labels (thin-label model,
//! ADR-011), counters stored raw with rates derived at query time (ADR-012), and an
//! explicit, exhaustive node state machine (monitoring-conventions).

pub mod address;
pub mod classification;
pub mod collection;
pub mod ids;
pub mod metric;
pub mod node;
pub mod profile;
pub mod rbac;
pub mod severity;
pub mod state;
pub mod thresholds;
pub mod url_check;

pub use address::AddressFamily;
pub use classification::{
    builtin_classification_rules, BuiltinClassificationRule, ClassificationRule,
};
pub use collection::{
    builtin_catalog, builtin_interface_meta_columns, builtin_profiles, builtin_templates,
    resolve_collection_set, BuiltinProfile, BuiltinTemplate, CollectionItem, CollectionKind,
    InterfaceField, ScopedCollectionItem, OID_IF_HIGH_SPEED, TEMPLATE_STANDARD_SNMP,
};
pub use ids::{CheckId, CredentialId, GroupId, IfIndex, NodeId, ProfileId};
pub use metric::{MetricKind, SeriesKey};
pub use node::Node;
pub use profile::{Profile, ProfileCategory};
pub use rbac::{Permission, Principal, Role, Scope};
pub use severity::Severity;
pub use state::NodeState;
pub use thresholds::{
    resolve_effective, Direction, EffectiveThreshold, ScopeLevel, ScopedThreshold, ThresholdRule,
};
pub use url_check::{
    is_ssrf_blocked, ExpectedStatus, HttpMethod, UrlCheckConfig, METRIC_HTTP_STATUS_CODE,
    METRIC_HTTP_UP, METRIC_SSL_CERT_DAYS_TO_EXPIRY,
};
