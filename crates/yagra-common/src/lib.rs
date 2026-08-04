// SPDX-License-Identifier: AGPL-3.0-only
//! yagra-common — shared types and models across Yagra components.
//!
//! Node/device-class, metric, alert-state, and severity types that more than one
//! component needs to agree on live here. Keep protocol- or store-specific types
//! out of this crate; this is the cross-cutting vocabulary only.
//!
//! Design anchors: stable IDs as the only series labels (thin-label model,
//! ADR-011), counters stored raw with rates derived at query time (ADR-012), and an
//! explicit, exhaustive node state machine (monitoring-conventions).

pub mod classification;
pub mod collection;
pub mod dns_check;
pub mod host;
pub mod ids;
pub mod l3;
pub mod meraki;
pub mod metric;
pub mod neighbor;
pub mod node;
pub mod node_kind;
pub mod notify_template;
pub mod profile;
pub mod rbac;
pub mod severity;
pub mod state;
pub mod thresholds;
pub mod topology;
pub mod trap;
pub mod url_check;

pub use classification::{
    builtin_classification_rules, BuiltinClassificationRule, ClassificationRule,
};
pub use collection::{
    builtin_catalog, builtin_interface_meta_columns, builtin_metric_kind, builtin_profiles,
    builtin_templates, resolve_collection_set, BuiltinProfile, BuiltinTemplate, CollectionItem,
    CollectionKind, InterfaceField, ScopedCollectionItem, OID_IF_HIGH_SPEED,
    TEMPLATE_STANDARD_SNMP,
};
pub use dns_check::{
    is_resolver_blocked, normalize_dns_name, validate_dns_name, DnsAnswer, DnsChain,
    DnsCheckConfig, DnsFailure, DnsFailureKind, DnsHop, DnsRecord, DnsRecordType,
    METRIC_DNS_ANSWER_COUNT, METRIC_DNS_CHAIN_LENGTH, METRIC_DNS_RESOLVE_MS, METRIC_DNS_UP,
};
pub use host::{DiskUsage, HostSample};
pub use ids::{CheckId, CredentialId, GroupId, IfIndex, NodeId, ProfileId};
pub use l3::{
    builtin_l3_columns, decode_prefix_pointer, inet_address_from_index, prefix_len_from_mask,
    subnet_key, L3AddrType, L3Address, L3Column, L3Snapshot, L3SourceTable, SubnetKey,
    MAX_ADDRESSES_PER_NODE, METRIC_SNMP_L3_ADDRESS_COUNT,
};
pub use meraki::{
    api_profile_name_for_product_type, category_for_product_type, is_meraki_api_host,
    uplink_ifindex, uplink_name, uplink_status_value, MerakiDeviceConfig, MerakiTier,
    METRIC_MERAKI_CLIENT_COUNT, METRIC_MERAKI_DEVICE_UP, METRIC_MERAKI_LAST_SEEN_SECS,
    METRIC_MERAKI_UPLINK_LATENCY_MS, METRIC_MERAKI_UPLINK_LOSS_PCT, METRIC_MERAKI_UPLINK_STATUS,
    METRIC_MERAKI_USAGE_RECV_KB, METRIC_MERAKI_USAGE_SENT_KB,
};
pub use metric::{is_valid_metric_name, MetricKind, SeriesKey};
pub use neighbor::{
    builtin_neighbor_columns, cdp_capabilities, lldp_capabilities, render_bare_address,
    render_chassis_id, render_hex, render_mac, render_network_address, render_port_id, render_text,
    Neighbor, NeighborCapability, NeighborColumn, NeighborProto, NeighborSet,
    MAX_NEIGHBORS_PER_NODE, METRIC_SNMP_NEIGHBOR_COUNT,
};
pub use node::Node;
pub use node_kind::{NodeKind, NodeRows};
pub use notify_template::{
    minimal_facts, sample_facts, AlertFacts, NotifyEvent, TemplateVariable, TEMPLATE_VARIABLES,
};
pub use profile::ProfileCategory;
pub use rbac::{Permission, Principal, Role, Scope, TokenSurface, UserKind};
pub use severity::Severity;
pub use state::NodeState;
pub use thresholds::{
    resolve_effective, Direction, EffectiveThreshold, ScopeLevel, ScopedThreshold, ThresholdRule,
};
pub use topology::{
    DerivedLink, LinkDirection, LinkOverride, LinkOverrideAction, LinkSource, TopologyLinkSummary,
    MAX_LINKS_PER_NODE,
};
pub use trap::trap_oid_name;
pub use url_check::{
    host_ip, is_ssrf_blocked, ExpectedStatus, HttpAuth, HttpMethod, UrlCheckConfig,
    HTTP_AUTH_SCHEMES, METRIC_HTTP_STATUS_CODE, METRIC_HTTP_UP, METRIC_SSL_CERT_DAYS_TO_EXPIRY,
};
