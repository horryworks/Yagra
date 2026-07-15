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
pub mod host;
pub mod ids;
pub mod meraki;
pub mod metric;
pub mod node;
pub mod profile;
pub mod rbac;
pub mod severity;
pub mod state;
pub mod thresholds;
pub mod trap;
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
pub use host::{DiskUsage, HostSample};
pub use ids::{CheckId, CredentialId, GroupId, IfIndex, NodeId, ProfileId};
pub use meraki::{
    api_profile_name_for_product_type, category_for_product_type, is_meraki_api_host,
    uplink_ifindex, uplink_name, uplink_status_value, MerakiDeviceConfig, MerakiTier,
    METRIC_MERAKI_CLIENT_COUNT, METRIC_MERAKI_DEVICE_UP, METRIC_MERAKI_LAST_SEEN_SECS,
    METRIC_MERAKI_UPLINK_LATENCY_MS, METRIC_MERAKI_UPLINK_LOSS_PCT, METRIC_MERAKI_UPLINK_STATUS,
    METRIC_MERAKI_USAGE_RECV_KB, METRIC_MERAKI_USAGE_SENT_KB, PROFILE_MERAKI_MR_API,
    PROFILE_MERAKI_MS_API, PROFILE_MERAKI_MX_API,
};
pub use metric::{is_valid_metric_name, MetricKind, SeriesKey};
pub use node::Node;
pub use profile::ProfileCategory;
pub use rbac::{Permission, Principal, Role, Scope};
pub use severity::Severity;
pub use state::NodeState;
pub use thresholds::{
    resolve_effective, Direction, EffectiveThreshold, ScopeLevel, ScopedThreshold, ThresholdRule,
};
pub use trap::trap_oid_name;
pub use url_check::{
    is_ssrf_blocked, ExpectedStatus, HttpMethod, UrlCheckConfig, METRIC_HTTP_STATUS_CODE,
    METRIC_HTTP_UP, METRIC_SSL_CERT_DAYS_TO_EXPIRY,
};
