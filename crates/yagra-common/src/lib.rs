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

pub mod arp;
pub mod classification;
pub mod collection;
pub mod dns_check;
pub mod host;
pub mod ids;
pub mod l3;
pub mod link_mode;
pub mod meraki;
pub mod metric;
pub mod neighbor;
pub mod node;
mod node_kind;
mod notify_template;
pub mod profile;
mod rbac;
pub mod routing;
pub mod severity;
pub mod state;
pub mod thresholds;
pub mod topology;
pub mod trap;
pub mod url_check;

pub use arp::{
    builtin_arp_columns, ArpColumn, ArpEntry, ArpInterfaceCount, ArpSummary,
    MAX_ARP_ENTRIES_PER_NODE, MAX_ARP_WALK_ROWS, METRIC_SNMP_ARP_ENTRY_COUNT,
};
pub use classification::{
    builtin_classification_rules, BuiltinClassificationRule, ClassificationRule,
};
pub use collection::{
    builtin_catalog, builtin_interface_meta_columns, builtin_metric_kind, builtin_profiles,
    builtin_templates, is_plausible_dbm, resolve_collection_set, BuiltinProfile, BuiltinTemplate,
    CollectionItem, CollectionKind, InterfaceField, OpticalFlavor, ScopedCollectionItem,
    METRIC_IF_RX_POWER_DBM, METRIC_IF_TX_POWER_DBM, OID_IF_HIGH_SPEED, OPTICAL_DBM_MAX,
    OPTICAL_DBM_MIN, TEMPLATE_STANDARD_SNMP,
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
    MAX_ADDRESSES_PER_NODE, MAX_L3_WALK_ROWS, METRIC_SNMP_L3_ADDRESS_COUNT,
};
pub use link_mode::{
    duplex_from_dot3, if_type_from_snmp, mau_subid, media_from_mau_oid,
    media_from_transceiver_text, Duplex, MauMedia, DOT3_MAU_TYPE_ROOT, IF_TYPE_ETHERNET_CSMACD,
    OID_DOT3_DUPLEX_STATUS, OID_IF_MAU_IFINDEX, OID_IF_MAU_TYPE, OID_IF_TYPE,
};
pub use meraki::{
    api_profile_name_for_product_type, category_for_product_type, is_meraki_api_host,
    uplink_ifindex, uplink_name, uplink_status_value, MerakiDeviceConfig, MerakiTier,
    METRIC_MERAKI_CLIENT_COUNT, METRIC_MERAKI_DEVICE_UP, METRIC_MERAKI_LAST_SEEN_SECS,
    METRIC_MERAKI_UPLINK_LATENCY_MS, METRIC_MERAKI_UPLINK_LOSS_PCT, METRIC_MERAKI_UPLINK_STATUS,
    METRIC_MERAKI_USAGE_RECV_KB, METRIC_MERAKI_USAGE_SENT_KB,
};
pub use metric::{is_valid_metric_name, MetricKind, SeriesKey, METRIC_ICMP_RTT_MS};
pub use neighbor::{
    builtin_neighbor_columns, cdp_capabilities, lldp_capabilities, render_bare_address,
    render_chassis_id, render_hex, render_mac, render_network_address, render_port_id, render_text,
    Neighbor, NeighborCapability, NeighborColumn, NeighborProto, NeighborSet,
    MAX_NEIGHBORS_PER_NODE, MAX_NEIGHBOR_WALK_ROWS, METRIC_SNMP_NEIGHBOR_COUNT,
};
pub use node::Node;
pub use node_kind::{NodeKind, NodeRows};
pub use notify_template::{
    minimal_facts, sample_facts, AlertFacts, NotifyEvent, TemplateVariable, TEMPLATE_VARIABLES,
};
pub use profile::ProfileCategory;
pub use rbac::{Permission, Principal, Role, Scope, TokenSurface, UserKind};
pub use routing::{
    bgp_mib_covers, bgp_peer_from_instance, builtin_routing_columns, host_prefix_len,
    ospf_neighbor_from_instance, route_prefix_len_from_instance, route_probe_columns,
    route_probe_oid, RoutingAdjacency, RoutingColumn, RoutingProto, RoutingSnapshot,
    INET_CIDR_ROUTE_TYPE_LOCAL, MAX_ROUTE_PROBES_PER_NODE, MAX_ROUTE_PROBE_ROWS,
    MAX_ROUTING_ADJACENCIES_PER_NODE, MAX_ROUTING_WALK_ROWS, METRIC_SNMP_ROUTING_ADJACENCY_COUNT,
};
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
    host_ip, is_ssrf_blocked, json_metric_value, resolve_json_path, url_monitor_reserved_metrics,
    BodyMatch, BodyMatchMode, ExpectedStatus, HttpAuth, HttpMethod, JsonExtract, UrlCheckConfig,
    BODY_MATCH_MODES, BODY_MAX_BYTES_RANGE, BODY_PATTERN_MAX_LEN, DEFAULT_BODY_MAX_BYTES,
    HTTP_AUTH_SCHEMES, JSON_PATH_MAX_LEN, MAX_JSON_EXTRACTS, METRIC_HTTP_BODY_MATCH,
    METRIC_HTTP_BODY_TRUNCATED, METRIC_HTTP_RESPONSE_TIME_MS, METRIC_HTTP_STATUS_CODE,
    METRIC_HTTP_UP, METRIC_NAME_MAX_LEN, METRIC_SSL_CERT_DAYS_TO_EXPIRY,
};
