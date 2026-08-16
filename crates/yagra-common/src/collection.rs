// SPDX-License-Identifier: AGPL-3.0-only
//! Collection sets: *what* Yagra collects from a node, and how those choices resolve.
//!
//! A **collection set** is the list of metrics polled from a node. Like thresholds
//! (ADR-013), items are defined at a scope — a device-class [`ScopeLevel::Profile`] or
//! directly on a node ([`ScopeLevel::Node`]) — and resolve by **inheritance with
//! override**: a node-level item replaces the profile-level item with the same
//! `metric_name`, and node-only items are added on top. This keeps "what to collect"
//! configurable per device class with per-node overrides.
//!
//! Two shapes of item exist (see [`CollectionKind`]): a **scalar** OID fetched with GET
//! (e.g. sysUpTime), and a **table** column base walked with GETBULK to yield one value
//! per interface row. Either way the `metric_name` is a *stable, bounded* identifier — it
//! becomes the TSDB metric label, so it must never be a free-text/device-supplied value,
//! or series cardinality explodes (monitoring-conventions, ADR-011).
//!
//! Resolution lives *only here* so precedence logic is never scattered (mirrors
//! [`crate::thresholds::resolve_effective`]).

use crate::metric::MetricKind;
use crate::profile::ProfileCategory;
use crate::thresholds::ScopeLevel;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How a [`CollectionItem`] is collected from the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CollectionKind {
    /// A single scalar instance OID fetched with SNMP GET (e.g. sysUpTime).
    Scalar,
    /// A table *column* base OID walked with GETBULK — one value per row/interface,
    /// keyed by the trailing sub-identifier (the ifIndex).
    Table,
    /// An optical-transceiver (DDM/DOM) probe: several correlated columns walked together and
    /// normalised to dBm, keyed by a **real** ifIndex (ADR-062).
    ///
    /// Not a [`CollectionKind::Table`] because one OID does not make one metric here. The
    /// standard spelling (ENTITY-SENSOR-MIB) needs value + scale + precision + type + a
    /// free-text description correlated across five columns before a single dBm reading exists,
    /// and most vendors key the result by `entPhysicalIndex` (the transceiver is a *part*, not a
    /// port) which must be mapped to an ifIndex before the sample means anything to the rest of
    /// the system. The poller owns all of that; see [`OpticalFlavor`].
    ///
    /// The item's `oid` is the flavour's **entry OID**, not a column base — it selects which
    /// vendor dialect to speak. An `oid` no [`OpticalFlavor::from_root`] recognises is skipped.
    Optical,
}

impl CollectionKind {
    /// Every variant, for exhaustive iteration in tests.
    pub const ALL: [Self; 3] = [Self::Scalar, Self::Table, Self::Optical];

    /// The token stored in the `collection` column of `collection_items` /
    /// `collection_template_items`.
    ///
    /// ⚠️ **This is a database value, so it is frozen.** Renaming one makes every existing row
    /// unreadable, and the column has no CHECK constraint to notice.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::Table => "table",
            Self::Optical => "optical",
        }
    }

    /// Parse the stored token back, or `None` when this binary does not know it.
    ///
    /// 🚨 **This function exists because its absence shipped a silent, total failure.** The reader
    /// used to be a `match` ending in `_ => CollectionKind::Scalar`, so the moment `optical` rows
    /// were seeded they read back as scalars: the scheduler put them in the SNMP **GET** list, no
    /// optical probe was ever built, and the metric inventory reported them as node-level. Nothing
    /// errored, nothing logged, and every test passed — because every test used the enum directly
    /// and never went through the database. A wildcard over a domain enum is exactly the failure
    /// `.claude/rules/extensibility.md` §1 forbids, and this is what it looks like in production.
    ///
    /// `None` rather than a default is the point: the caller has to decide what an unknown token
    /// means, and "an older binary reading a newer row" is a real case — a rollback after this
    /// shipped will find `optical` rows it cannot run.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// Which vendor dialect an [`CollectionKind::Optical`] item speaks, selected by the item's `oid`.
///
/// Every dialect answers the same two questions — "what is this port receiving" and "what is it
/// transmitting", in dBm — and differs only in where the numbers live, how they are scaled, and
/// what indexes them. Keeping the dialect in an enum rather than in extra `CollectionItem`
/// columns is what lets a new vendor be one arm here instead of a schema migration (ADR-046
/// decision 6 declined a `unit` column; this ADR does not reopen it — the unit is in the metric
/// *name*, `if_rx_power_dbm`, exactly as `icmp_rtt_ms` does it).
///
/// ⚠️ **Only the dialects whose scale factor was verified against the vendor MIB are here.**
/// MikroTik's `mtxrOpticalTable` was left out deliberately: the column numbers were confirmed but
/// the scale was not, and a wrong scale produces a plausible-looking wrong number, which is worse
/// than no chart at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpticalFlavor {
    /// ENTITY-SENSOR-MIB (RFC 3433) — Cisco, Arista, and most vendors that implement ENTITY-MIB.
    /// The only vendor-neutral spelling, and the most expensive to read: the sensor rows carry a
    /// *type*, a *scale* and a *precision* that must be combined, and nothing but the physical
    /// entity's free-text name says whether a row is the transmit or the receive sensor.
    EntitySensor,
    /// HUAWEI-ENTITY-EXTENT-MIB `hwOpticalModuleInfoTable` — Rx `.8`, Tx `.9`, both dBm × 100,
    /// indexed by `entPhysicalIndex`.
    Huawei,
    /// JUNIPER-DOM-MIB `jnxDomCurrentTable` — Rx `.5`, Tx `.7`, both dBm × 100, and **indexed by
    /// ifIndex directly**, so no entity mapping is needed.
    Juniper,
    /// HH3C-TRANSCEIVER-INFO-MIB `hh3cTransceiverInfoTable` — Tx `.11`, Rx `.12`, both dBm × 100,
    /// indexed by ifIndex. Covers H3C and the HPE Comware switches OEMed from it.
    H3c,
}

impl OpticalFlavor {
    /// Every dialect, for exhaustive iteration in tests and seeding.
    pub const ALL: [Self; 4] = [Self::EntitySensor, Self::Huawei, Self::Juniper, Self::H3c];

    /// The entry OID that selects this dialect — the value an [`CollectionKind::Optical`] item
    /// carries in its `oid` field.
    #[must_use]
    pub const fn root_oid(self) -> &'static str {
        match self {
            Self::EntitySensor => "1.3.6.1.2.1.99.1.1.1",
            Self::Huawei => "1.3.6.1.4.1.2011.5.25.31.1.1.3.1",
            Self::Juniper => "1.3.6.1.4.1.2636.3.18.1.1.1",
            Self::H3c => "1.3.6.1.4.1.25506.2.70.1.1.1",
        }
    }

    /// The dialect an item's `oid` selects, or `None` when nothing recognises it.
    ///
    /// `None` is not an error: an operator may have typed an OID by hand, and an older core may
    /// have seeded a dialect this binary predates. The poller skips it rather than failing the job.
    #[must_use]
    pub fn from_root(oid: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.root_oid() == oid)
    }

    /// Whether the dialect's rows are already keyed by ifIndex.
    ///
    /// `false` means the rows are keyed by `entPhysicalIndex` and must be translated through
    /// ENTITY-MIB's `entAliasMappingIdentifier` before they can be attached to an interface —
    /// a row that does not translate is **dropped**, never emitted under its raw index (ADR-062
    /// decision 3): an untranslated row lands on `MetricDimension::Entity`, which means it costs
    /// a series and appears on no chart.
    #[must_use]
    pub const fn is_ifindex_keyed(self) -> bool {
        match self {
            Self::Juniper | Self::H3c => true,
            Self::EntitySensor | Self::Huawei => false,
        }
    }

    /// Display name of the built-in collection template that carries this dialect.
    #[must_use]
    pub const fn template_name(self) -> &'static str {
        match self {
            Self::EntitySensor => T_OPTICAL_STD,
            Self::Huawei => T_OPTICAL_HUAWEI,
            Self::Juniper => T_OPTICAL_JUNIPER,
            Self::H3c => T_OPTICAL_H3C,
        }
    }
}

/// Receive optical power, dBm. **The module's reading, not a per-lane one** — a multi-lane QSFP
/// reports one figure per module here, and a future per-lane metric gets its own name rather than
/// silently redefining this one (the `in_ucast_pps` lesson from ADR-060 decision 3).
pub const METRIC_IF_RX_POWER_DBM: &str = "if_rx_power_dbm";

/// Transmit optical power, dBm. Module-level, with the same caveat as [`METRIC_IF_RX_POWER_DBM`].
pub const METRIC_IF_TX_POWER_DBM: &str = "if_tx_power_dbm";

/// The plausible range of a real optical power reading, in dBm.
///
/// Transceivers live between roughly −40 dBm (below any receiver's sensitivity — what a dark fibre
/// reports) and +10 dBm (above any pluggable's launch power). A normalised value outside this is
/// not a dim link, it is a **scale that was applied wrongly**, and dropping it is the honest
/// response: a wrong-by-100× number plots as a smooth, believable line.
pub const OPTICAL_DBM_MIN: f64 = -60.0;
/// Upper end of [`OPTICAL_DBM_MIN`]'s plausible window.
pub const OPTICAL_DBM_MAX: f64 = 20.0;

/// Whether a normalised dBm reading is inside the plausible window.
#[must_use]
pub fn is_plausible_dbm(v: f64) -> bool {
    v.is_finite() && (OPTICAL_DBM_MIN..=OPTICAL_DBM_MAX).contains(&v)
}

/// One thing to collect: a stable metric name, the OID to collect it from, how to collect
/// it (scalar GET vs table walk), and whether it is a gauge or a raw counter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CollectionItem {
    /// Stable TSDB metric name (e.g. `if_hc_in_octets`). Bounded by convention to keep
    /// label cardinality controlled — never a free-text/device-supplied value (ADR-011).
    pub metric_name: String,
    /// Dotted OID: a scalar instance OID ([`CollectionKind::Scalar`]) or a table column
    /// base ([`CollectionKind::Table`], the walk root).
    pub oid: String,
    /// Scalar GET vs table walk.
    pub kind: CollectionKind,
    /// Gauge vs raw counter — rates/utilization are derived at query time (ADR-012),
    /// never by the poller.
    pub metric_kind: MetricKind,
}

/// A [`CollectionItem`] tagged with the scope it was defined at, for resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopedCollectionItem {
    /// Scope this item was defined at.
    pub level: ScopeLevel,
    /// The item itself.
    pub item: CollectionItem,
}

impl ScopedCollectionItem {
    /// Convenience constructor.
    #[must_use]
    pub fn new(level: ScopeLevel, item: CollectionItem) -> Self {
        Self { level, item }
    }
}

/// An interface-metadata field discovered alongside the numeric table columns.
///
/// These are *not* time-series — they are descriptive attributes that live in PostgreSQL
/// and are joined to interface metrics at query time (thin-label model, ADR-011). They are
/// walked from the agent but never become TSDB labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceField {
    /// `ifName` — short interface name (e.g. `Gi0/1`).
    Name,
    /// `ifAlias` — operator-assigned description.
    Alias,
    /// Effective line rate in bits per second. The wire column carries the `ifSpeed` (32-bit)
    /// OID; the poller folds in `ifHighSpeed` ([`OID_IF_HIGH_SPEED`]) so links faster than the
    /// 32-bit `ifSpeed` cap (~4.29 Gbps) report their true rate. Resolved poller-side so the bus
    /// contract stays unchanged (N/N-1 compatible) — no separate `HighSpeed` field on the wire.
    Speed,
}

/// Resolve a set of scoped items into the **effective** collection set for a node.
///
/// Precedence is per `metric_name`: the item at the most-specific [`ScopeLevel`] present
/// wins (Node > Group > Profile), so a node-level entry overrides the profile default and
/// node-only entries are added. Output is sorted by `metric_name` for determinism. Items
/// flagged disabled are filtered out *before* calling this (callers pass only enabled rows).
#[must_use]
pub fn resolve_collection_set(items: &[ScopedCollectionItem]) -> Vec<CollectionItem> {
    // metric_name -> (winning level so far, item). A strictly more-specific level replaces.
    let mut winners: BTreeMap<&str, (ScopeLevel, &CollectionItem)> = BTreeMap::new();
    for sci in items {
        let key = sci.item.metric_name.as_str();
        match winners.get(key) {
            Some((level, _)) if *level >= sci.level => {}
            _ => {
                winners.insert(key, (sci.level, &sci.item));
            }
        }
    }
    winners
        .into_values()
        .map(|(_, item)| item.clone())
        .collect()
}

// --- Built-in standard catalog ------------------------------------------------------
//
// Applied to every SNMP-enabled node that has no explicit collection set, so behaviour is
// useful out of the box (and the legacy "poll sysUpTime" default is preserved). Every
// metric_name here is a fixed identifier — the bounded set is what keeps TSDB cardinality
// controlled.

/// sysUpTime.0 — system uptime in hundredths of a second (scalar). Module-internal: used by the
/// built-in catalog builder below; not part of the crate's public OID surface (cf. the re-exported
/// [`OID_IF_HIGH_SPEED`], which the poller references directly).
const OID_SYS_UPTIME: &str = "1.3.6.1.2.1.1.3.0";

/// hrProcessorLoad — HOST-RESOURCES-MIB per-processor load %, walked as a table column.
/// The standard cross-vendor CPU metric (net-snmp/host agents); network gear that lacks
/// HOST-RESOURCES simply returns no rows for it (skipped by the poller). Module-internal (see
/// [`OID_SYS_UPTIME`]).
const OID_HR_PROCESSOR_LOAD: &str = "1.3.6.1.2.1.25.3.3.1.2";

/// ifHighSpeed — ifXTable interface speed in **units of 1,000,000 bits/sec** (Mbps). Required
/// for links faster than the 32-bit `ifSpeed` gauge can report (it saturates at 4,294,967,295,
/// ~4.29 Gbps). The poller walks this alongside `ifSpeed` to resolve the effective bandwidth
/// stored in `interfaces.if_speed` (see [`InterfaceField::Speed`]).
pub const OID_IF_HIGH_SPEED: &str = "1.3.6.1.2.1.31.1.1.1.15";

/// The standard scalar + interface-table metrics collected by default.
///
/// Scalar: sysUpTime. Table (per-interface, ifXTable/ifTable columns): 64-bit octet
/// counters, 64-bit unicast packet counters, error counters, discard counters, oper status, and
/// high-speed. Counters are stored **raw**; utilization is derived from `rate()` at query time
/// (ADR-012).
#[must_use]
pub fn builtin_catalog() -> Vec<CollectionItem> {
    let scalar = |metric: &str, oid: &str, mk: MetricKind| CollectionItem {
        metric_name: metric.to_owned(),
        oid: oid.to_owned(),
        kind: CollectionKind::Scalar,
        metric_kind: mk,
    };
    let table = |metric: &str, oid: &str, mk: MetricKind| CollectionItem {
        metric_name: metric.to_owned(),
        oid: oid.to_owned(),
        kind: CollectionKind::Table,
        metric_kind: mk,
    };
    vec![
        scalar("snmp_sys_uptime_ticks", OID_SYS_UPTIME, MetricKind::Gauge),
        // ifXTable high-capacity octet counters (64-bit) — preferred over ifInOctets.
        table(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            MetricKind::Counter,
        ),
        table(
            "if_hc_out_octets",
            "1.3.6.1.2.1.31.1.1.1.10",
            MetricKind::Counter,
        ),
        // ifXTable high-capacity unicast *packet* counters (64-bit), the pps counterpart to the
        // octet counters above — a device's forwarding ceiling is often a packet rate, not a bit
        // rate, so a link with bandwidth to spare can still be saturated (ADR-060). Same table as
        // the octets, so this costs no vendor coverage. Unicast only, deliberately: multicast and
        // broadcast (.8/.9/.12/.13) would triple the added series for a fraction of the traffic on
        // a normal link, and cardinality is the single biggest design risk (ADR-011). The API
        // spells the limitation into the field name (`in_ucast_pps`) rather than hiding it, so a
        // later total can be *added* instead of silently redefining an existing number.
        table(
            "if_hc_in_ucast_pkts",
            "1.3.6.1.2.1.31.1.1.1.7",
            MetricKind::Counter,
        ),
        table(
            "if_hc_out_ucast_pkts",
            "1.3.6.1.2.1.31.1.1.1.11",
            MetricKind::Counter,
        ),
        // ifTable error + discard counters. Discards (congestion/queue drops) complement the
        // error counters and are stored raw; rates are derived via rate() at query time.
        table("if_in_errors", "1.3.6.1.2.1.2.2.1.14", MetricKind::Counter),
        table("if_out_errors", "1.3.6.1.2.1.2.2.1.20", MetricKind::Counter),
        table(
            "if_in_discards",
            "1.3.6.1.2.1.2.2.1.13",
            MetricKind::Counter,
        ),
        table(
            "if_out_discards",
            "1.3.6.1.2.1.2.2.1.19",
            MetricKind::Counter,
        ),
        // ifOperStatus / ifAdminStatus (1=up) and ifHighSpeed (Mbps) as gauges. Admin status lets
        // alerting tell an intentionally-shut port (admin-down) from an unplanned outage.
        table("if_oper_status", "1.3.6.1.2.1.2.2.1.8", MetricKind::Gauge),
        table("if_admin_status", "1.3.6.1.2.1.2.2.1.7", MetricKind::Gauge),
        table("if_high_speed", OID_IF_HIGH_SPEED, MetricKind::Gauge),
        // hrProcessorLoad — standard per-CPU load % (HOST-RESOURCES-MIB). Best-effort across
        // vendors; absent on devices without HOST-RESOURCES (returns no rows). Surfaced as a
        // node-level CPU% via the query-time `max()` aggregate.
        table(
            "hr_processor_load",
            OID_HR_PROCESSOR_LOAD,
            MetricKind::Gauge,
        ),
    ]
}

/// The declared [`MetricKind`] of a built-in catalog metric, or `None` when `name` is not
/// in the built-in catalog. This is how the API layer asks "is this metric a raw counter?"
/// without re-spelling the catalog's names anywhere else.
#[must_use]
pub fn builtin_metric_kind(name: &str) -> Option<MetricKind> {
    builtin_catalog()
        .into_iter()
        .find(|i| i.metric_name == name)
        .map(|i| i.metric_kind)
}

/// The standard interface-metadata columns (ifName, ifAlias, ifSpeed) and their OID bases.
///
/// Walked alongside the numeric columns to populate the PostgreSQL `interfaces` table; these
/// never become TSDB series. The `Speed` column sends the 32-bit `ifSpeed` OID, but the poller
/// also walks [`OID_IF_HIGH_SPEED`] and resolves the **effective** bandwidth (so >4.29 Gbps
/// links are correct) before storing it — see [`InterfaceField::Speed`]. The OID list here is
/// the bus contract; keep it stable for N/N-1 compatibility.
#[must_use]
pub fn builtin_interface_meta_columns() -> Vec<(InterfaceField, &'static str)> {
    vec![
        (InterfaceField::Name, "1.3.6.1.2.1.31.1.1.1.1"),
        (InterfaceField::Alias, "1.3.6.1.2.1.31.1.1.1.18"),
        (InterfaceField::Speed, "1.3.6.1.2.1.2.2.1.5"),
    ]
}

// --- Built-in collection templates + device profiles --------------------------------
//
// Collection templates are reusable, named metric bundles (the design's middle layer:
// MIB → Collection templates → Device profile references). A device profile is just a set
// of attached templates (it holds no raw OIDs). Core seeds these templates and links the
// built-in profiles to them, so binding a profile (+ an SNMP credential) yields graphs out
// of the box. The generic interface set works on virtually any SNMP agent; the vendor
// columns are *best-effort* common OIDs (walked as table columns so we don't guess an
// instance index) — an OID a device lacks is simply skipped by the poller.

/// A named, reusable collection template and the metrics it bundles.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinTemplate {
    /// Display name (unique).
    pub name: &'static str,
    /// One-line description.
    pub description: &'static str,
    /// The metrics this template collects.
    pub items: Vec<CollectionItem>,
}

/// A built-in device profile: a name, its functional category + vendor, and the template
/// names it references (it holds no raw OIDs — templates do).
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinProfile {
    /// Display name shown in Device profiles.
    pub name: &'static str,
    /// Functional role — the primary split axis / UI grouping.
    pub category: ProfileCategory,
    /// Vendor label (descriptive metadata only — never a TSDB label).
    pub vendor: Option<&'static str>,
    /// Names of the [`builtin_templates`] this profile attaches (empty ⇒ ICMP-only).
    pub templates: Vec<&'static str>,
}

/// A vendor table column (CPU/mem usage etc.), walked per-entity like the interface table.
fn vendor_table(metric: &str, oid: &str) -> CollectionItem {
    CollectionItem {
        metric_name: metric.to_owned(),
        oid: oid.to_owned(),
        kind: CollectionKind::Table,
        metric_kind: MetricKind::Gauge,
    }
}

/// A vendor scalar gauge (a `.0`-instance OID fetched with GET — e.g. FortiGate's
/// `fgSysCpuUsage`). The OID must include the trailing instance (usually `.0`).
fn vendor_scalar(metric: &str, oid: &str) -> CollectionItem {
    CollectionItem {
        metric_name: metric.to_owned(),
        oid: oid.to_owned(),
        kind: CollectionKind::Scalar,
        metric_kind: MetricKind::Gauge,
    }
}

/// A vendor scalar **counter** (a `.0`-instance counter OID — e.g. IPsec global octets). Stored
/// raw; bandwidth/throughput is derived via `rate()` at query time (ADR-012), never by the poller.
fn vendor_scalar_counter(metric: &str, oid: &str) -> CollectionItem {
    CollectionItem {
        metric_name: metric.to_owned(),
        oid: oid.to_owned(),
        kind: CollectionKind::Scalar,
        metric_kind: MetricKind::Counter,
    }
}

// ── Template names (referenced by both templates and profiles below) ────────────────
// Three tiers: role-base (vendor-neutral standard MIBs), vendor-health (per-NOS CPU/mem/
// temp), and role-KPI (functional: firewall sessions etc.). A profile composes one base +
// (optionally) a vendor-health + a role-KPI template. Every name is unique.

/// Standard SNMP template name — the cross-vendor system + interface set every SNMP profile
/// builds on (sysUpTime + interface counters/errors/status + generic `hrProcessorLoad`).
pub const TEMPLATE_STANDARD_SNMP: &str = "Standard SNMP";

// Role-base (vendor-neutral).
const T_HOST_RESOURCES: &str = "Host resources";
const T_ENTITY_SENSORS: &str = "Entity sensors";
const T_BGP: &str = "BGP peers";
const T_UPS: &str = "UPS (RFC1628)";
const T_PRINTER: &str = "Printer (Printer-MIB)";

// Vendor-health (per NOS).
const T_CISCO_IOS: &str = "Cisco IOS/IOS-XE health";
const T_CISCO_NXOS: &str = "Cisco NX-OS health";
const T_CISCO_XR: &str = "Cisco IOS-XR health";
const T_CISCO_ASA: &str = "Cisco ASA health";
const T_JUNIPER: &str = "Juniper Junos health";
const T_HUAWEI: &str = "Huawei VRP health";
const T_FORTINET: &str = "Fortinet FortiOS health";
const T_MIKROTIK: &str = "MikroTik RouterOS health";
const T_NETSNMP: &str = "Linux Net-SNMP (UCD)";

// Role-KPI (functional).
const T_ASA_SESSIONS: &str = "Cisco ASA sessions";
const T_HUAWEI_USG_SESSIONS: &str = "Huawei USG sessions";

// Role-KPI (VPN) — firewalls used as VPN heads. Cisco RA/IPsec reuse across ASA + FTD (+ IOS
// IPsec gateways); Fortinet/Palo Alto carry their own VPN counters.
const T_CISCO_RA_VPN: &str = "Cisco remote-access VPN";
const T_CISCO_IPSEC: &str = "Cisco IPsec tunnels";
const T_FORTINET_VPN: &str = "Fortinet VPN";
const T_PANOS: &str = "Palo Alto sessions/VPN";

// ── Optical transceiver (DDM/DOM) — one per vendor dialect (ADR-062) ────────────────
// A fourth tier. Split by dialect rather than folded into the vendor-health templates above
// because the *reading* differs, not just the OID: see `OpticalFlavor`. Each carries the same
// two metric names, so a node bound to two of them by mistake still produces one series pair.
const T_OPTICAL_STD: &str = "Optical transceivers (ENTITY-SENSOR)";
const T_OPTICAL_HUAWEI: &str = "Optical transceivers (Huawei)";
const T_OPTICAL_JUNIPER: &str = "Optical transceivers (Juniper)";
const T_OPTICAL_H3C: &str = "Optical transceivers (H3C/Comware)";

/// The built-in collection templates shipped with Yagra (seeded on startup).
///
/// Three tiers (see the name constants above). Vendor OIDs are best-effort common columns
/// walked per-entity; an OID a device lacks is simply skipped by the poller. Long-tail
/// vendors carry only the standard-MIB templates — vendor OIDs are added later as data via
/// the template CRUD, no code change.
#[must_use]
pub fn builtin_templates() -> Vec<BuiltinTemplate> {
    vec![
        // ── Role-base (vendor-neutral standard MIBs) ──
        BuiltinTemplate {
            name: TEMPLATE_STANDARD_SNMP,
            description:
                "System uptime + per-interface traffic (bits and packets), errors, discards, \
                 and status (any SNMP agent).",
            items: builtin_catalog(),
        },
        BuiltinTemplate {
            name: T_HOST_RESOURCES,
            description: "HOST-RESOURCES-MIB storage used/size and process count, plus TCP-MIB \
                 established connections (servers, hosts, NAS).",
            items: vec![
                vendor_table("hr_storage_used", "1.3.6.1.2.1.25.2.3.1.6"),
                vendor_table("hr_storage_size", "1.3.6.1.2.1.25.2.3.1.5"),
                vendor_scalar("hr_system_processes", "1.3.6.1.2.1.25.1.6.0"),
                // TCP-MIB tcpCurrEstab — current established TCP connections; a vendor-neutral
                // load signal available on virtually every host/appliance SNMP agent.
                vendor_scalar("tcp_curr_estab", "1.3.6.1.2.1.6.9.0"),
            ],
        },
        BuiltinTemplate {
            name: T_ENTITY_SENSORS,
            description: "ENTITY-SENSOR-MIB physical sensor values (temperature/fan/voltage).",
            items: vec![vendor_table("ent_sensor_value", "1.3.6.1.2.1.99.1.1.1.4")],
        },
        BuiltinTemplate {
            name: T_BGP,
            description: "BGP4-MIB per-peer session and admin state (routers).",
            items: vec![
                vendor_table("bgp_peer_state", "1.3.6.1.2.1.15.3.1.2"),
                vendor_table("bgp_peer_admin_status", "1.3.6.1.2.1.15.3.1.3"),
            ],
        },
        BuiltinTemplate {
            name: T_UPS,
            description:
                "UPS-MIB (RFC1628) battery status, runtime/charge remaining, and output load.",
            items: vec![
                vendor_scalar("ups_battery_status", "1.3.6.1.2.1.33.1.2.1.0"),
                vendor_scalar("ups_minutes_remaining", "1.3.6.1.2.1.33.1.2.3.0"),
                vendor_scalar("ups_charge_remaining_pct", "1.3.6.1.2.1.33.1.2.4.0"),
                vendor_table("ups_output_load_pct", "1.3.6.1.2.1.33.1.4.4.1.5"),
            ],
        },
        BuiltinTemplate {
            name: T_PRINTER,
            description: "Printer-MIB marker page count and alert severity (network printers).",
            items: vec![
                vendor_table("prt_marker_life_count", "1.3.6.1.2.1.43.10.2.1.4"),
                vendor_table("prt_alert_severity", "1.3.6.1.2.1.43.18.1.1.2"),
            ],
        },
        // ── Vendor-health (per NOS) ──
        BuiltinTemplate {
            name: T_CISCO_IOS,
            description: "Cisco IOS/IOS-XE CPU (cpmCPUTotal5minRev), memory-pool used/free, temp.",
            items: vec![
                vendor_table("cisco_cpu_5min", "1.3.6.1.4.1.9.9.109.1.1.1.1.8"),
                vendor_table("cisco_mem_used", "1.3.6.1.4.1.9.9.48.1.1.1.5"),
                vendor_table("cisco_mem_free", "1.3.6.1.4.1.9.9.48.1.1.1.6"),
                vendor_table("cisco_env_temp", "1.3.6.1.4.1.9.9.13.1.3.1.3"),
            ],
        },
        BuiltinTemplate {
            name: T_CISCO_NXOS,
            description: "Cisco NX-OS CPU and memory utilization % (CISCO-SYSTEM-EXT-MIB).",
            items: vec![
                vendor_scalar("nxos_cpu_util", "1.3.6.1.4.1.9.9.305.1.1.1.0"),
                vendor_scalar("nxos_mem_util", "1.3.6.1.4.1.9.9.305.1.1.2.0"),
            ],
        },
        BuiltinTemplate {
            name: T_CISCO_XR,
            description: "Cisco IOS-XR CPU (cpmCPUTotal5minRev) and enhanced memory-pool used.",
            items: vec![
                vendor_table("cisco_cpu_5min", "1.3.6.1.4.1.9.9.109.1.1.1.1.8"),
                vendor_table("cisco_cemp_mem_used", "1.3.6.1.4.1.9.9.221.1.1.1.1.18"),
            ],
        },
        BuiltinTemplate {
            name: T_CISCO_ASA,
            description: "Cisco ASA CPU (cpmCPUTotal5minRev) and enhanced memory-pool used.",
            items: vec![
                vendor_table("cisco_cpu_5min", "1.3.6.1.4.1.9.9.109.1.1.1.1.8"),
                vendor_table("cisco_cemp_mem_used", "1.3.6.1.4.1.9.9.221.1.1.1.1.18"),
            ],
        },
        BuiltinTemplate {
            name: T_JUNIPER,
            description: "Juniper jnxOperating CPU (1-min), buffer utilization %, and temperature.",
            items: vec![
                vendor_table("juniper_cpu_1min", "1.3.6.1.4.1.2636.3.1.13.1.8"),
                vendor_table("juniper_buffer_util", "1.3.6.1.4.1.2636.3.1.13.1.11"),
                vendor_table("juniper_temp", "1.3.6.1.4.1.2636.3.1.13.1.7"),
            ],
        },
        BuiltinTemplate {
            name: T_HUAWEI,
            description:
                "Huawei VRP entity CPU/memory usage %, RAM total+free bytes, and temperature.",
            items: vec![
                vendor_table("huawei_cpu_usage", "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.5"),
                vendor_table("huawei_mem_usage", "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.7"),
                // HUAWEI-MEMORY-MIB hwMemoryDevTable, 64-bit columns = actual RAM bytes, so the UI
                // can show used/total ("2.6 GB / 3.4 GB"). The entity-extent hwEntityMemSize (.6)
                // returns a bogus small value on YunShan OS, so we don't use it. used = total − free.
                vendor_table("huawei_mem_total", "1.3.6.1.4.1.2011.6.3.5.1.1.8"),
                vendor_table("huawei_mem_free", "1.3.6.1.4.1.2011.6.3.5.1.1.9"),
                vendor_table("huawei_temp", "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.11"),
            ],
        },
        BuiltinTemplate {
            name: T_FORTINET,
            description: "FortiGate system CPU/memory usage % and active session count.",
            items: vec![
                vendor_scalar("fortinet_cpu_usage", "1.3.6.1.4.1.12356.101.4.1.3.0"),
                vendor_scalar("fortinet_mem_usage", "1.3.6.1.4.1.12356.101.4.1.4.0"),
                vendor_scalar("fortinet_sessions", "1.3.6.1.4.1.12356.101.4.1.8.0"),
            ],
        },
        BuiltinTemplate {
            name: T_MIKROTIK,
            description: "MikroTik RouterOS processor temperature and voltage (mtxrHealth).",
            items: vec![
                vendor_scalar("mikrotik_cpu_temp", "1.3.6.1.4.1.14988.1.1.3.10.0"),
                vendor_scalar("mikrotik_voltage", "1.3.6.1.4.1.14988.1.1.3.8.0"),
            ],
        },
        BuiltinTemplate {
            name: T_NETSNMP,
            description: "Linux/Unix Net-SNMP (UCD) CPU idle %, real-memory total/available, \
                 load averages, swap, and per-disk usage %.",
            items: vec![
                vendor_scalar("ucd_cpu_idle_pct", "1.3.6.1.4.1.2021.11.11.0"),
                vendor_scalar("ucd_mem_total_kb", "1.3.6.1.4.1.2021.4.5.0"),
                vendor_scalar("ucd_mem_avail_kb", "1.3.6.1.4.1.2021.4.6.0"),
                // laLoadInt (load average ×100 as an integer) for the 1/5/15-min instances — the
                // classic Unix saturation signal. Stored as-is; the UI scales ÷100 to a real load.
                vendor_scalar("ucd_load_1min", "1.3.6.1.4.1.2021.10.1.5.1"),
                vendor_scalar("ucd_load_5min", "1.3.6.1.4.1.2021.10.1.5.2"),
                vendor_scalar("ucd_load_15min", "1.3.6.1.4.1.2021.10.1.5.3"),
                // Swap total/available (KB) and per-mount disk usage % (dskTable column, walked).
                vendor_scalar("ucd_swap_total_kb", "1.3.6.1.4.1.2021.4.3.0"),
                vendor_scalar("ucd_swap_avail_kb", "1.3.6.1.4.1.2021.4.4.0"),
                vendor_table("ucd_disk_used_pct", "1.3.6.1.4.1.2021.9.1.9"),
            ],
        },
        // ── Role-KPI (functional) ──
        BuiltinTemplate {
            name: T_ASA_SESSIONS,
            description: "Cisco ASA current connection count (CISCO-FIREWALL-MIB).",
            items: vec![vendor_table(
                "asa_current_connections",
                "1.3.6.1.4.1.9.9.147.1.2.2.2.1.5",
            )],
        },
        BuiltinTemplate {
            name: T_HUAWEI_USG_SESSIONS,
            description: "Huawei USG firewall current total session count and session setup rate \
                 (HUAWEI-SECURITY-STAT-MIB).",
            // Both are gauges, not counters: the MIB types them Counter64 but they are
            // instantaneous (rise and fall), so they're stored as-is with no rate() at query time.
            items: vec![
                vendor_table(
                    "huawei_usg_total_sessions",
                    "1.3.6.1.4.1.2011.6.122.15.1.2.1.4",
                ),
                vendor_table(
                    "huawei_usg_session_setup_rate",
                    "1.3.6.1.4.1.2011.6.122.15.1.2.1.3",
                ),
            ],
        },
        // ── Role-KPI (VPN) — appended at end so existing template seed-ids don't shift ──
        BuiltinTemplate {
            name: T_CISCO_RA_VPN,
            description:
                "Cisco remote-access VPN (AnyConnect/SSL/IPsec-RA) active sessions, users, \
                 and traffic (CISCO-REMOTE-ACCESS-MONITOR-MIB) — ASA/FTD VPN concentrators.",
            // Counts are gauges; the octet totals are counters (bandwidth via rate() at query time).
            items: vec![
                vendor_scalar("cisco_ra_sessions", "1.3.6.1.4.1.9.9.392.1.3.1.0"),
                vendor_scalar("cisco_ra_users", "1.3.6.1.4.1.9.9.392.1.3.3.0"),
                vendor_scalar_counter("cisco_ra_in_octets", "1.3.6.1.4.1.9.9.392.1.3.7.0"),
                vendor_scalar_counter("cisco_ra_out_octets", "1.3.6.1.4.1.9.9.392.1.3.9.0"),
            ],
        },
        BuiltinTemplate {
            name: T_CISCO_IPSEC,
            description: "Cisco site-to-site IPsec/IKE active tunnel counts and 64-bit throughput \
                 (CISCO-IPSEC-FLOW-MONITOR-MIB) — ASA and IOS/IOS-XR IPsec gateways.",
            items: vec![
                vendor_scalar("cisco_ike_active_tunnels", "1.3.6.1.4.1.9.9.171.1.2.1.1.0"),
                vendor_scalar(
                    "cisco_ipsec_active_tunnels",
                    "1.3.6.1.4.1.9.9.171.1.3.1.1.0",
                ),
                vendor_scalar_counter("cisco_ipsec_in_octets", "1.3.6.1.4.1.9.9.171.1.3.1.4.0"),
                vendor_scalar_counter("cisco_ipsec_out_octets", "1.3.6.1.4.1.9.9.171.1.3.1.17.0"),
            ],
        },
        BuiltinTemplate {
            name: T_FORTINET_VPN,
            description:
                "FortiGate IPsec tunnels up and SSL-VPN logged-in users (FORTINET-FORTIGATE-MIB).",
            items: vec![
                vendor_scalar("fortinet_vpn_tunnels_up", "1.3.6.1.4.1.12356.101.12.1.1.0"),
                // SSL-VPN logged-in users is a per-VDOM table column — walked, collapsed node-wide.
                vendor_table("fortinet_sslvpn_users", "1.3.6.1.4.1.12356.101.12.2.3.1.2"),
            ],
        },
        BuiltinTemplate {
            name: T_PANOS,
            description: "Palo Alto PAN-OS active sessions, session-table utilization %, and \
                 GlobalProtect connected tunnels (PAN-COMMON-MIB).",
            items: vec![
                vendor_scalar("panos_sessions_active", "1.3.6.1.4.1.25461.2.1.2.3.1.0"),
                vendor_scalar("panos_session_util_pct", "1.3.6.1.4.1.25461.2.1.2.3.2.0"),
                vendor_scalar("panos_gp_active_tunnels", "1.3.6.1.4.1.25461.2.1.2.5.1.3.0"),
            ],
        },
        // ── Optical transceivers (ADR-062) ──
        // ⚠️ APPEND-ONLY, and this is the one array in this file where that matters: `repo.rs`
        // derives each template's seed id from its **index here** (`SeedRange::CollectionTemplates
        // .id(i)`), so inserting above re-keys every template after it and silently breaks the
        // profile→template links of existing deployments. `builtin_profiles` has an end-of-array
        // guard test; this array has none, so the discipline is on the reader.
        //
        // Each template's items are `Optical`, and their `oid` is the dialect's **entry** OID, not
        // a column base — one item does not map to one column here (see `CollectionKind::Optical`).
        // Two items per dialect so the inventory stays honest: `list_node_metrics` joins configured
        // items against real series by metric name, and a single "if_optical_power" family row
        // would report `no_data` forever while the two real series reported `unconfigured`.
        optical_template(OpticalFlavor::EntitySensor),
        optical_template(OpticalFlavor::Huawei),
        optical_template(OpticalFlavor::Juniper),
        optical_template(OpticalFlavor::H3c),
    ]
}

/// The built-in optical template for one dialect: the same two metric names, pointed at the
/// dialect's entry OID.
fn optical_template(flavor: OpticalFlavor) -> BuiltinTemplate {
    let item = |metric: &str| CollectionItem {
        metric_name: metric.to_owned(),
        oid: flavor.root_oid().to_owned(),
        kind: CollectionKind::Optical,
        metric_kind: MetricKind::Gauge,
    };
    BuiltinTemplate {
        name: flavor.template_name(),
        description: match flavor {
            OpticalFlavor::EntitySensor => {
                "Transceiver receive/transmit optical power in dBm, read from ENTITY-SENSOR-MIB \
                 (RFC 3433) and attached to interfaces through ENTITY-MIB's alias mapping. The \
                 vendor-neutral spelling — Cisco, Arista and most implementers of ENTITY-MIB."
            }
            OpticalFlavor::Huawei => {
                "Transceiver receive/transmit optical power in dBm, read from Huawei's \
                 hwOpticalModuleInfoTable and attached to interfaces through ENTITY-MIB's alias \
                 mapping."
            }
            OpticalFlavor::Juniper => {
                "Transceiver receive/transmit optical power in dBm, read from JUNIPER-DOM-MIB. \
                 Indexed by ifIndex directly, so no entity mapping is needed."
            }
            OpticalFlavor::H3c => {
                "Transceiver receive/transmit optical power in dBm, read from \
                 HH3C-TRANSCEIVER-INFO-MIB. Covers H3C and the HPE Comware switches OEMed from \
                 it; devices without the MIB simply return no rows."
            }
        },
        items: vec![item(METRIC_IF_RX_POWER_DBM), item(METRIC_IF_TX_POWER_DBM)],
    }
}

/// The built-in device profiles: split by **functional role (category) × vendor-NOS family**,
/// each composing reusable templates (role-base + optional vendor-health + role-KPI). The
/// catalog is rebuilt cleanly (the migration reseeds it), so unlike before there is no
/// append-only constraint — order is by category for readability. Major vendors carry real
/// vendor-health/role-KPI OIDs; long-tail devices carry the standard-MIB base, with vendor OIDs
/// added later as data. Every SNMP profile attaches "Standard SNMP" so it graphs out of the box.
///
/// **Stable name required:** "Generic SNMP" is the classifier's SNMP fallback (looked up by
/// name) and "Generic ping" the ICMP-only default — keep both names.
#[must_use]
pub fn builtin_profiles() -> Vec<BuiltinProfile> {
    let prof = |name: &'static str,
                category: ProfileCategory,
                vendor: Option<&'static str>,
                templates: Vec<&'static str>| BuiltinProfile {
        name,
        category,
        vendor,
        templates,
    };
    use ProfileCategory as C;
    vec![
        // ── Routers ──
        prof(
            "Cisco IOS/IOS-XE router",
            C::Router,
            Some("Cisco"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_CISCO_IOS,
                T_ENTITY_SENSORS,
                T_BGP,
                T_OPTICAL_STD,
            ],
        ),
        prof(
            "Cisco IOS-XR router",
            C::Router,
            Some("Cisco"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_CISCO_XR,
                T_ENTITY_SENSORS,
                T_BGP,
                T_OPTICAL_STD,
            ],
        ),
        prof(
            "Juniper MX router",
            C::Router,
            Some("Juniper"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_JUNIPER,
                T_ENTITY_SENSORS,
                T_BGP,
                T_OPTICAL_JUNIPER,
            ],
        ),
        prof(
            "Huawei NE/AR router",
            C::Router,
            Some("Huawei"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_HUAWEI,
                T_ENTITY_SENSORS,
                T_BGP,
                T_OPTICAL_HUAWEI,
            ],
        ),
        prof(
            "Nokia SR router",
            C::Router,
            Some("Nokia"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_ENTITY_SENSORS,
                T_BGP,
                T_OPTICAL_STD,
            ],
        ),
        prof(
            "MikroTik RouterOS",
            C::Router,
            Some("MikroTik"),
            vec![TEMPLATE_STANDARD_SNMP, T_MIKROTIK],
        ),
        prof(
            "Generic router",
            C::Router,
            None,
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_ENTITY_SENSORS,
                T_BGP,
                T_OPTICAL_STD,
            ],
        ),
        // ── Switches (L3 / L2) ──
        prof(
            "Cisco Catalyst switch (IOS/IOS-XE)",
            C::L3Switch,
            Some("Cisco"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_CISCO_IOS,
                T_ENTITY_SENSORS,
                T_OPTICAL_STD,
            ],
        ),
        prof(
            "Cisco Nexus switch (NX-OS)",
            C::L3Switch,
            Some("Cisco"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_CISCO_NXOS,
                T_ENTITY_SENSORS,
                T_OPTICAL_STD,
            ],
        ),
        prof(
            "Arista EOS switch",
            C::L3Switch,
            Some("Arista"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_HOST_RESOURCES,
                T_ENTITY_SENSORS,
                T_OPTICAL_STD,
            ],
        ),
        prof(
            "Juniper EX/QFX switch",
            C::L3Switch,
            Some("Juniper"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_JUNIPER,
                T_ENTITY_SENSORS,
                T_OPTICAL_JUNIPER,
            ],
        ),
        prof(
            "Huawei CloudEngine switch",
            C::L3Switch,
            Some("Huawei"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_HUAWEI,
                T_ENTITY_SENSORS,
                T_OPTICAL_HUAWEI,
            ],
        ),
        prof(
            "Aruba/HPE switch",
            C::L3Switch,
            Some("Aruba/HPE"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_ENTITY_SENSORS,
                T_HOST_RESOURCES,
                T_OPTICAL_H3C,
            ],
        ),
        prof(
            "Dell switch",
            C::L3Switch,
            Some("Dell"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_ENTITY_SENSORS,
                T_HOST_RESOURCES,
                T_OPTICAL_STD,
            ],
        ),
        prof(
            "Extreme EXOS switch",
            C::L3Switch,
            Some("Extreme Networks"),
            vec![TEMPLATE_STANDARD_SNMP, T_ENTITY_SENSORS, T_OPTICAL_STD],
        ),
        prof(
            "Brocade / Ruckus switch",
            C::L2Switch,
            Some("Brocade"),
            vec![TEMPLATE_STANDARD_SNMP, T_ENTITY_SENSORS],
        ),
        prof(
            "Ubiquiti switch/AP",
            C::L2Switch,
            Some("Ubiquiti"),
            vec![TEMPLATE_STANDARD_SNMP],
        ),
        prof(
            "NETGEAR switch",
            C::L2Switch,
            Some("NETGEAR"),
            vec![TEMPLATE_STANDARD_SNMP],
        ),
        prof(
            "D-Link switch",
            C::L2Switch,
            Some("D-Link"),
            vec![TEMPLATE_STANDARD_SNMP],
        ),
        prof(
            "TP-Link switch",
            C::L2Switch,
            Some("TP-Link"),
            vec![TEMPLATE_STANDARD_SNMP],
        ),
        prof(
            "Zyxel switch",
            C::L2Switch,
            Some("Zyxel"),
            vec![TEMPLATE_STANDARD_SNMP],
        ),
        prof(
            "Generic switch",
            C::L2Switch,
            None,
            vec![TEMPLATE_STANDARD_SNMP, T_ENTITY_SENSORS, T_OPTICAL_STD],
        ),
        // ── Firewalls ──
        prof(
            "Cisco ASA firewall",
            C::Firewall,
            Some("Cisco"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_CISCO_ASA,
                T_ASA_SESSIONS,
                T_CISCO_RA_VPN,
                T_CISCO_IPSEC,
                T_ENTITY_SENSORS,
                T_OPTICAL_STD,
            ],
        ),
        prof(
            "Cisco Firepower (FTD)",
            C::Firewall,
            Some("Cisco"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_HOST_RESOURCES,
                T_CISCO_RA_VPN,
                T_ENTITY_SENSORS,
            ],
        ),
        prof(
            "Fortinet FortiGate",
            C::Firewall,
            Some("Fortinet"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_FORTINET,
                T_FORTINET_VPN,
                T_ENTITY_SENSORS,
            ],
        ),
        prof(
            "Palo Alto PAN-OS firewall",
            C::Firewall,
            Some("Palo Alto"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_HOST_RESOURCES,
                T_PANOS,
                T_ENTITY_SENSORS,
            ],
        ),
        prof(
            "Check Point firewall",
            C::Firewall,
            Some("Check Point"),
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES, T_ENTITY_SENSORS],
        ),
        prof(
            "Juniper SRX firewall",
            C::Firewall,
            Some("Juniper"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_JUNIPER,
                T_ENTITY_SENSORS,
                T_OPTICAL_JUNIPER,
            ],
        ),
        prof(
            "Huawei USG firewall",
            C::Firewall,
            Some("Huawei"),
            vec![
                TEMPLATE_STANDARD_SNMP,
                T_HUAWEI,
                T_HUAWEI_USG_SESSIONS,
                T_ENTITY_SENSORS,
                T_OPTICAL_HUAWEI,
            ],
        ),
        prof(
            "Generic firewall",
            C::Firewall,
            None,
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES, T_ENTITY_SENSORS],
        ),
        // ── Wireless ──
        prof(
            "Cisco wireless controller",
            C::WirelessController,
            Some("Cisco"),
            vec![TEMPLATE_STANDARD_SNMP],
        ),
        prof(
            "Aruba wireless controller",
            C::WirelessController,
            Some("Aruba"),
            vec![TEMPLATE_STANDARD_SNMP],
        ),
        prof(
            "Generic wireless AP",
            C::WirelessAp,
            None,
            vec![TEMPLATE_STANDARD_SNMP],
        ),
        // ── Load balancers / ADC ──
        prof(
            "F5 BIG-IP",
            C::LoadBalancer,
            Some("F5"),
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES],
        ),
        prof(
            "Citrix ADC (NetScaler)",
            C::LoadBalancer,
            Some("Citrix"),
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES],
        ),
        prof(
            "Generic load balancer",
            C::LoadBalancer,
            None,
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES],
        ),
        // ── Servers ──
        prof(
            "Linux server (Net-SNMP)",
            C::Server,
            Some("Linux"),
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES, T_NETSNMP],
        ),
        prof(
            "Windows server",
            C::Server,
            Some("Microsoft"),
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES],
        ),
        prof(
            "Unix server",
            C::Server,
            None,
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES],
        ),
        prof(
            "Generic server",
            C::Server,
            None,
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES],
        ),
        // ── Hypervisors ──
        prof(
            "VMware ESXi host",
            C::Hypervisor,
            Some("VMware"),
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES],
        ),
        // ── Storage / NAS ──
        prof(
            "Synology NAS",
            C::Storage,
            Some("Synology"),
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES],
        ),
        prof(
            "QNAP NAS",
            C::Storage,
            Some("QNAP"),
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES],
        ),
        prof(
            "NetApp / generic storage",
            C::Storage,
            Some("NetApp"),
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES],
        ),
        // ── Power (UPS / PDU) ──
        prof(
            "APC UPS",
            C::Power,
            Some("APC"),
            vec![TEMPLATE_STANDARD_SNMP, T_UPS],
        ),
        prof(
            "Generic UPS (RFC1628)",
            C::Power,
            None,
            vec![TEMPLATE_STANDARD_SNMP, T_UPS],
        ),
        prof("Generic PDU", C::Power, None, vec![TEMPLATE_STANDARD_SNMP]),
        // ── Printers ──
        prof(
            "Network printer",
            C::Printer,
            None,
            vec![TEMPLATE_STANDARD_SNMP, T_PRINTER],
        ),
        // ── Generic / fallback (keep these names — classifier looks them up) ──
        prof(
            "Generic SNMP",
            C::GenericSnmp,
            None,
            vec![TEMPLATE_STANDARD_SNMP],
        ),
        prof("Generic ping", C::PingOnly, None, Vec::new()),
        // ── Appended post-redesign — kept at array end on purpose ──
        // Seed ids are Uuid::from_u128(BASE + index); inserting mid-array would shift every later
        // profile's id and duplicate its row on reseed (ON CONFLICT DO NOTHING). Appending gives
        // these fresh ids with zero churn. The UI groups by `category`, not array order, so each
        // still appears in its proper section (Wireless controller / Load balancer).
        prof(
            "Huawei wireless controller",
            C::WirelessController,
            Some("Huawei"),
            vec![TEMPLATE_STANDARD_SNMP, T_HUAWEI],
        ),
        prof(
            "A10 Thunder ADC",
            C::LoadBalancer,
            Some("A10 Networks"),
            vec![TEMPLATE_STANDARD_SNMP, T_HOST_RESOURCES],
        ),
        // Cisco Meraki (cloud-managed): local per-device SNMP exposes standard MIBs only (IF-MIB +
        // system uptime) — no vendor CPU/mem and no VPN/AutoVPN (those live in the Dashboard API,
        // not SNMP). So both carry just the Standard SNMP base: interface traffic/errors/status +
        // ICMP liveness. MX = security appliance (firewall), MS = switch.
        prof(
            "Cisco Meraki MX",
            C::Firewall,
            Some("Cisco Meraki"),
            vec![TEMPLATE_STANDARD_SNMP],
        ),
        prof(
            "Cisco Meraki MS switch",
            C::L2Switch,
            Some("Cisco Meraki"),
            vec![TEMPLATE_STANDARD_SNMP],
        ),
        // URL / HTTP(S) endpoint monitor. Polled over HTTP (status/up + cert expiry), not SNMP —
        // so it carries no collection templates; the per-node URL config drives the probe and the
        // profile only exists to group these nodes and host their default thresholds. Kept at the
        // array end for seed-id stability (see the note above).
        prof("URL / HTTP endpoint", C::UrlCheck, None, Vec::new()),
        // Cisco Meraki (Dashboard API) profiles — distinct from the SNMP "Cisco Meraki MX/MS"
        // profiles above. These devices are polled over the cloud Dashboard API (org-scoped
        // collector), not SNMP, so they carry NO collection templates; the per-org collector emits
        // the metrics and the profile only groups these nodes + hosts their default thresholds
        // (device up / uplink loss/latency, seeded in core). Kept at the array end for seed-id
        // stability. Roles mirror category_for_product_type(): MX→firewall, MS→switch, MR→AP.
        prof(
            crate::meraki::PROFILE_MERAKI_MX_API,
            C::Firewall,
            Some("Cisco Meraki"),
            Vec::new(),
        ),
        prof(
            crate::meraki::PROFILE_MERAKI_MS_API,
            C::L2Switch,
            Some("Cisco Meraki"),
            Vec::new(),
        ),
        prof(
            crate::meraki::PROFILE_MERAKI_MR_API,
            C::WirelessAp,
            Some("Cisco Meraki"),
            Vec::new(),
        ),
        // DNS name-resolution monitor (ADR-033). Polled over DNS (resolve + CNAME-chain walk), not
        // SNMP — so it carries no collection templates; the per-node DNS config drives the probe and
        // the profile only exists to group these nodes and host their default threshold (`dns_up`).
        // Kept at the array end for seed-id stability (see the note above).
        prof("DNS name resolution", C::DnsCheck, None, Vec::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_metric_kind_reads_the_catalog_not_a_second_list() {
        // Counters and gauges resolve from the one catalog; an unknown name is None, not a guess.
        assert_eq!(
            builtin_metric_kind("if_hc_in_octets"),
            Some(MetricKind::Counter)
        );
        assert_eq!(
            builtin_metric_kind("if_in_errors"),
            Some(MetricKind::Counter)
        );
        // sysUpTime is deliberately a gauge (reboot detection via `below` must keep working).
        assert_eq!(
            builtin_metric_kind("snmp_sys_uptime_ticks"),
            Some(MetricKind::Gauge)
        );
        assert_eq!(builtin_metric_kind("icmp_rtt_ms"), None);
    }

    #[test]
    fn builtin_profile_names_are_unique() {
        let names: Vec<&str> = builtin_profiles().into_iter().map(|p| p.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate built-in profile name");
    }

    #[test]
    fn newest_builtin_profile_stays_at_the_array_end() {
        // Seed ids are Uuid::from_u128(BASE + index), so inserting a profile mid-array silently
        // re-keys every profile after it. New profiles must be appended; this pins the most
        // recently added one (DNS, ADR-033) so a future insertion trips here instead of in prod.
        let profiles = builtin_profiles();
        assert_eq!(
            profiles.last().map(|p| p.name),
            Some("DNS name resolution"),
            "append new built-in profiles; never insert mid-array"
        );
    }

    fn item(metric: &str, oid: &str) -> CollectionItem {
        CollectionItem {
            metric_name: metric.to_owned(),
            oid: oid.to_owned(),
            kind: CollectionKind::Scalar,
            metric_kind: MetricKind::Gauge,
        }
    }

    #[test]
    fn node_overrides_profile_for_same_metric() {
        // Profile and Node both define cpu_util; the node-level OID must win.
        let items = [
            ScopedCollectionItem::new(ScopeLevel::Profile, item("cpu_util", "1.1.1")),
            ScopedCollectionItem::new(ScopeLevel::Node, item("cpu_util", "2.2.2")),
        ];
        let resolved = resolve_collection_set(&items);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].oid, "2.2.2");
    }

    #[test]
    fn node_only_items_are_added_on_top_of_profile() {
        let items = [
            ScopedCollectionItem::new(ScopeLevel::Profile, item("a", "1")),
            ScopedCollectionItem::new(ScopeLevel::Node, item("b", "2")),
        ];
        let resolved = resolve_collection_set(&items);
        let names: Vec<&str> = resolved.iter().map(|i| i.metric_name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]); // sorted by metric_name, both present
    }

    #[test]
    fn profile_default_used_when_no_node_override() {
        let items = [ScopedCollectionItem::new(
            ScopeLevel::Profile,
            item("cpu_util", "1.1.1"),
        )];
        let resolved = resolve_collection_set(&items);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].oid, "1.1.1");
    }

    #[test]
    fn empty_resolves_to_empty() {
        assert!(resolve_collection_set(&[]).is_empty());
    }

    #[test]
    fn builtin_catalog_has_scalar_and_table_items_with_bounded_names() {
        let cat = builtin_catalog();
        assert!(cat
            .iter()
            .any(|i| i.metric_name == "snmp_sys_uptime_ticks" && i.kind == CollectionKind::Scalar));
        let octets = cat
            .iter()
            .find(|i| i.metric_name == "if_hc_in_octets")
            .expect("hc in octets present");
        assert_eq!(octets.kind, CollectionKind::Table);
        assert_eq!(octets.metric_kind, MetricKind::Counter);
    }

    #[test]
    fn builtin_catalog_includes_standard_cpu_load() {
        let cat = builtin_catalog();
        let cpu = cat
            .iter()
            .find(|i| i.metric_name == "hr_processor_load")
            .expect("hr_processor_load present in Standard SNMP");
        assert_eq!(cpu.oid, OID_HR_PROCESSOR_LOAD);
        assert_eq!(cpu.kind, CollectionKind::Table);
        assert_eq!(cpu.metric_kind, MetricKind::Gauge);
    }

    /// The pps counterpart to the octet counters (ADR-060). Pinned on three axes because each one
    /// is a different silent failure: a `Gauge` kind would make the API draw an odometer as if it
    /// were a rate, a `Scalar` kind would collect one value per *node* instead of per interface,
    /// and an OID outside `1.3.6.1.2.1.31.1.1.1` (ifXTable) would be a different table's column —
    /// none of which fails to compile, and none of which is visible on a lab device that happens
    /// to answer anyway.
    #[test]
    fn builtin_catalog_includes_unicast_packet_counters() {
        let cat = builtin_catalog();
        for (metric, oid) in [
            ("if_hc_in_ucast_pkts", "1.3.6.1.2.1.31.1.1.1.7"),
            ("if_hc_out_ucast_pkts", "1.3.6.1.2.1.31.1.1.1.11"),
        ] {
            let item = cat
                .iter()
                .find(|i| i.metric_name == metric)
                .unwrap_or_else(|| panic!("{metric} present in Standard SNMP"));
            assert_eq!(item.oid, oid, "{metric} OID");
            assert_eq!(
                item.kind,
                CollectionKind::Table,
                "{metric} is per-interface"
            );
            assert_eq!(
                item.metric_kind,
                MetricKind::Counter,
                "{metric} is a raw counter (rate() at query time, ADR-012)"
            );
        }
    }

    /// Every optical dialect ships a template, and every one of those templates collects exactly
    /// the two metric names the poller knows how to publish.
    ///
    /// The pairing is what keeps the metric inventory honest: `list_node_metrics` joins configured
    /// items against real series **by name**, so a template naming anything else would report
    /// `no_data` forever beside two series reporting `unconfigured`.
    #[test]
    fn every_optical_dialect_has_a_template_carrying_both_readings() {
        let templates = builtin_templates();
        for flavor in OpticalFlavor::ALL {
            let t = templates
                .iter()
                .find(|t| t.name == flavor.template_name())
                .unwrap_or_else(|| panic!("no template for {flavor:?}"));
            let mut names: Vec<&str> = t.items.iter().map(|i| i.metric_name.as_str()).collect();
            names.sort_unstable();
            assert_eq!(names, [METRIC_IF_RX_POWER_DBM, METRIC_IF_TX_POWER_DBM]);
            for item in &t.items {
                assert_eq!(item.kind, CollectionKind::Optical, "{flavor:?}");
                // Optical power is a level, never an odometer. A counter here would make the API
                // serve it through rate(), plotting the change in a logarithm per second.
                assert_eq!(item.metric_kind, MetricKind::Gauge, "{flavor:?}");
                // The item's OID selects the dialect, so it must be the root the poller matches on
                // and not one of the underlying columns.
                assert_eq!(item.oid, flavor.root_oid(), "{flavor:?}");
            }
        }
    }

    /// ⚠️ The optical templates must stay at the END of `builtin_templates()`.
    ///
    /// `repo.rs` derives a template's seed id from its index in this array, so inserting one above
    /// re-keys every template after it and silently breaks the profile→template links of every
    /// existing deployment. `builtin_profiles` has an end-of-array guard; this array had none, and
    /// this is it.
    #[test]
    fn optical_templates_stay_at_the_end_of_the_array() {
        let templates = builtin_templates();
        let tail: Vec<&str> = templates
            .iter()
            .rev()
            .take(OpticalFlavor::ALL.len())
            .map(|t| t.name)
            .collect();
        for flavor in OpticalFlavor::ALL {
            assert!(
                tail.contains(&flavor.template_name()),
                "{flavor:?}'s template moved out of the array tail — seed ids are array positions"
            );
        }
    }

    /// Every profile that attaches an optical template attaches exactly one.
    ///
    /// Two dialects on one node is not a crash — the poller runs both and publishes whichever
    /// answers — but it is always a mistake in the catalog: a box speaks one transceiver MIB, and
    /// the second walk is a per-poll SNMP session spent on a table that will never have rows.
    #[test]
    fn no_builtin_profile_attaches_two_optical_dialects() {
        let optical: Vec<&str> = OpticalFlavor::ALL
            .iter()
            .map(|f| f.template_name())
            .collect();
        for p in builtin_profiles() {
            let n = p.templates.iter().filter(|t| optical.contains(t)).count();
            assert!(n <= 1, "{} attaches {n} optical templates", p.name);
        }
    }

    /// A plausibility window that rejected everything would satisfy any "bad values are dropped"
    /// test while silently discarding every real reading, so the accepting cases are pinned too.
    #[test]
    fn the_plausible_dbm_window_admits_real_readings_and_rejects_scale_errors() {
        for ok in [-40.0, -20.0, -7.42, 0.0, 3.0] {
            assert!(is_plausible_dbm(ok), "{ok} should be plausible");
        }
        for bad in [-742.0, 742.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!is_plausible_dbm(bad), "{bad} should be refused");
        }
    }

    /// Every kind survives the trip through its **stored** token.
    ///
    /// 🚨 This is the test whose absence let ADR-062 ship inert. The reader in `yagra-core` ended
    /// in `_ => Scalar`, so `optical` rows came back as scalars and no optical probe was ever
    /// built — silently, with every other test green, because they all used the enum directly and
    /// never went through the database. A round trip is the only shape that catches it.
    #[test]
    fn every_collection_kind_round_trips_through_its_stored_token() {
        for kind in CollectionKind::ALL {
            assert_eq!(
                CollectionKind::from_token(kind.as_str()),
                Some(kind),
                "{kind:?} does not survive its own token"
            );
        }
        // Distinct tokens, or two kinds would read back as one.
        let mut tokens: Vec<&str> = CollectionKind::ALL.iter().map(|k| k.as_str()).collect();
        tokens.sort_unstable();
        let before = tokens.len();
        tokens.dedup();
        assert_eq!(before, tokens.len(), "two kinds share a stored token");
        // An unknown token is refused rather than defaulted — the caller decides what it means.
        assert_eq!(CollectionKind::from_token("something_new"), None);
        assert_eq!(CollectionKind::from_token(""), None);
    }

    /// The stored token and the JSON tag are produced by two different mechanisms — `as_str` and
    /// `#[serde(rename_all)]` — and nothing makes them agree. They must, because the same value is
    /// both a database column and an API field.
    #[test]
    fn the_stored_token_and_the_serde_tag_agree_for_every_kind() {
        for kind in CollectionKind::ALL {
            let json = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(
                json,
                format!("\"{}\"", kind.as_str()),
                "{kind:?}: stored token and serde tag disagree"
            );
        }
    }

    #[test]
    fn collection_kind_serde_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&CollectionKind::Table).unwrap(),
            "\"table\""
        );
        assert_eq!(
            serde_json::from_str::<CollectionKind>("\"scalar\"").unwrap(),
            CollectionKind::Scalar
        );
    }

    #[test]
    fn builtin_templates_carry_expected_metrics() {
        let templates = builtin_templates();
        let by_name = |n: &str| templates.iter().find(|t| t.name == n).unwrap();

        // Standard SNMP = the system + interface set.
        let std = &by_name(TEMPLATE_STANDARD_SNMP).items;
        assert!(std.iter().any(|i| i.metric_name == "snmp_sys_uptime_ticks"));
        assert!(std.iter().any(|i| i.metric_name == "if_hc_in_octets"
            && i.kind == CollectionKind::Table
            && i.metric_kind == MetricKind::Counter));

        // Vendor templates carry only their extras (not the generic set).
        let cisco = &by_name("Cisco IOS/IOS-XE health").items;
        assert!(cisco.iter().any(|i| i.metric_name == "cisco_cpu_5min"));
        assert!(!cisco.iter().any(|i| i.metric_name == "if_hc_in_octets"));
        let huawei = &by_name("Huawei VRP health").items;
        assert!(huawei.iter().any(|i| i.metric_name == "huawei_cpu_usage"));
        assert!(huawei.iter().any(|i| i.metric_name == "huawei_mem_usage"));
        // RAM total/free come from HUAWEI-MEMORY-MIB (bytes) so the UI shows used/total; the
        // bogus entity-extent size metric must be gone.
        assert!(huawei.iter().any(|i| i.metric_name == "huawei_mem_total"
            && i.kind == CollectionKind::Table
            && i.metric_kind == MetricKind::Gauge));
        assert!(huawei
            .iter()
            .any(|i| i.metric_name == "huawei_mem_free" && i.metric_kind == MetricKind::Gauge));
        assert!(!huawei.iter().any(|i| i.metric_name == "huawei_mem_size"));

        // USG firewall sessions = current total sessions + setup rate, both table gauges
        // (instantaneous, never counters — see the template description).
        let usg = &by_name(T_HUAWEI_USG_SESSIONS).items;
        assert_eq!(usg.len(), 2);
        assert!(usg
            .iter()
            .any(|i| i.metric_name == "huawei_usg_total_sessions"
                && i.kind == CollectionKind::Table
                && i.metric_kind == MetricKind::Gauge));
        assert!(usg
            .iter()
            .any(|i| i.metric_name == "huawei_usg_session_setup_rate"
                && i.metric_kind == MetricKind::Gauge));
    }

    #[test]
    fn builtin_profiles_reference_existing_templates() {
        let profiles = builtin_profiles();
        let by_name = |n: &str| profiles.iter().find(|p| p.name == n);

        // The classifier resolves these two by name — they must stay present.
        assert!(
            by_name("Generic ping").is_some_and(|p| p.templates.is_empty()),
            "Generic ping must exist and be ICMP-only"
        );
        assert_eq!(
            by_name("Generic SNMP").map(|p| p.templates.clone()),
            Some(vec![TEMPLATE_STANDARD_SNMP])
        );
        // A representative split profile composes its vendor-health template.
        assert!(by_name("Cisco Catalyst switch (IOS/IOS-XE)")
            .unwrap()
            .templates
            .contains(&T_CISCO_IOS));

        // Every referenced template actually exists.
        let template_names: std::collections::BTreeSet<&str> =
            builtin_templates().iter().map(|t| t.name).collect();
        for p in &profiles {
            for t in &p.templates {
                assert!(
                    template_names.contains(t),
                    "{} references unknown template {t}",
                    p.name
                );
            }
        }
    }

    #[test]
    fn appended_profiles_have_expected_category_and_templates() {
        let profiles = builtin_profiles();
        let by_name = |n: &str| profiles.iter().find(|p| p.name == n);

        let wac = by_name("Huawei wireless controller").expect("Huawei WAC profile present");
        assert_eq!(wac.category, ProfileCategory::WirelessController);
        assert_eq!(wac.vendor, Some("Huawei"));
        assert!(wac.templates.contains(&T_HUAWEI), "WAC carries VRP health");

        let a10 = by_name("A10 Thunder ADC").expect("A10 load-balancer profile present");
        assert_eq!(a10.category, ProfileCategory::LoadBalancer);
        assert_eq!(a10.vendor, Some("A10 Networks"));
        assert!(a10.templates.contains(&T_HOST_RESOURCES));
    }

    #[test]
    fn every_profile_has_templates_except_ping_only() {
        for p in builtin_profiles() {
            // ICMP-only (ping), URL/HTTP monitors, DNS monitors, and Cisco Meraki *Dashboard API*
            // profiles carry no SNMP collection templates — the metrics come from the per-node
            // config / the org collector, not an OID set. (The SNMP "Cisco Meraki MX/MS" profiles
            // still do.)
            if matches!(
                p.category,
                ProfileCategory::PingOnly | ProfileCategory::UrlCheck | ProfileCategory::DnsCheck
            ) || p.name.ends_with("(API)")
            {
                assert!(
                    p.templates.is_empty(),
                    "{} should carry no SNMP templates",
                    p.name
                );
            } else {
                assert!(
                    !p.templates.is_empty(),
                    "{} (SNMP) must attach at least one template",
                    p.name
                );
                assert!(
                    p.templates.contains(&TEMPLATE_STANDARD_SNMP),
                    "{} must build on the standard base",
                    p.name
                );
            }
        }
    }

    #[test]
    fn builtin_template_oids_are_dotted_digits() {
        // Mirror the API's is_valid_oid so seeded items can never be rejected on re-create.
        let dotted_digits = |oid: &str| {
            !oid.is_empty()
                && oid
                    .split('.')
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        };
        for t in builtin_templates() {
            for item in t.items {
                assert!(
                    dotted_digits(&item.oid),
                    "bad OID {} in {}",
                    item.oid,
                    t.name
                );
            }
        }
    }

    #[test]
    fn vpn_templates_carry_expected_metrics() {
        let templates = builtin_templates();
        let by_name = |n: &str| templates.iter().find(|t| t.name == n).unwrap();

        // Cisco remote-access VPN: session/user gauges + octet counters (all scalar GETs).
        let ra = &by_name(T_CISCO_RA_VPN).items;
        assert!(ra.iter().any(|i| i.metric_name == "cisco_ra_sessions"
            && i.kind == CollectionKind::Scalar
            && i.metric_kind == MetricKind::Gauge));
        assert!(
            ra.iter()
                .any(|i| i.metric_name == "cisco_ra_in_octets"
                    && i.metric_kind == MetricKind::Counter)
        );

        // Cisco IPsec tunnels: active-tunnel gauges + 64-bit throughput counters.
        let ipsec = &by_name(T_CISCO_IPSEC).items;
        assert!(ipsec
            .iter()
            .any(|i| i.metric_name == "cisco_ipsec_active_tunnels"
                && i.metric_kind == MetricKind::Gauge));
        assert!(ipsec
            .iter()
            .any(|i| i.metric_name == "cisco_ipsec_out_octets"
                && i.metric_kind == MetricKind::Counter));

        // Fortinet VPN: scalar tunnels-up + per-VDOM SSL-VPN users table.
        let fgt = &by_name(T_FORTINET_VPN).items;
        assert!(fgt.iter().any(
            |i| i.metric_name == "fortinet_vpn_tunnels_up" && i.kind == CollectionKind::Scalar
        ));
        assert!(fgt
            .iter()
            .any(|i| i.metric_name == "fortinet_sslvpn_users" && i.kind == CollectionKind::Table));

        // Palo Alto sessions/VPN: active sessions + GlobalProtect tunnels.
        let pan = &by_name(T_PANOS).items;
        assert!(pan.iter().any(|i| i.metric_name == "panos_sessions_active"));
        assert!(pan
            .iter()
            .any(|i| i.metric_name == "panos_gp_active_tunnels"));
    }

    #[test]
    fn firewall_profiles_attach_vpn_and_sensor_templates() {
        let profiles = builtin_profiles();
        let by_name = |n: &str| profiles.iter().find(|p| p.name == n).unwrap();

        let asa = &by_name("Cisco ASA firewall").templates;
        assert!(
            asa.contains(&T_CISCO_RA_VPN) && asa.contains(&T_CISCO_IPSEC),
            "ASA gains both VPN templates"
        );
        assert!(asa.contains(&T_ENTITY_SENSORS), "ASA gains entity sensors");
        assert!(by_name("Cisco Firepower (FTD)")
            .templates
            .contains(&T_CISCO_RA_VPN));
        assert!(by_name("Fortinet FortiGate")
            .templates
            .contains(&T_FORTINET_VPN));
        assert!(by_name("Palo Alto PAN-OS firewall")
            .templates
            .contains(&T_PANOS));
    }

    #[test]
    fn standard_host_and_ucd_templates_gained_p3_metrics() {
        let templates = builtin_templates();
        let by_name = |n: &str| templates.iter().find(|t| t.name == n).unwrap();

        let std = &by_name(TEMPLATE_STANDARD_SNMP).items;
        for m in ["if_in_discards", "if_out_discards", "if_admin_status"] {
            assert!(
                std.iter().any(|i| i.metric_name == m),
                "Standard SNMP missing {m}"
            );
        }
        assert!(by_name(T_HOST_RESOURCES)
            .items
            .iter()
            .any(|i| i.metric_name == "tcp_curr_estab"));
        let ucd = &by_name(T_NETSNMP).items;
        for m in ["ucd_load_1min", "ucd_swap_total_kb", "ucd_disk_used_pct"] {
            assert!(ucd.iter().any(|i| i.metric_name == m), "UCD missing {m}");
        }
    }

    #[test]
    fn meraki_profiles_present_and_snmp_only() {
        let profiles = builtin_profiles();
        let by_name = |n: &str| profiles.iter().find(|p| p.name == n);

        let mx = by_name("Cisco Meraki MX").expect("Meraki MX profile present");
        assert_eq!(mx.category, ProfileCategory::Firewall);
        assert_eq!(mx.vendor, Some("Cisco Meraki"));
        // Cloud-managed: Standard SNMP only — VPN/AutoVPN is Dashboard-API, not SNMP.
        assert_eq!(mx.templates, vec![TEMPLATE_STANDARD_SNMP]);

        let ms = by_name("Cisco Meraki MS switch").expect("Meraki MS profile present");
        assert_eq!(ms.category, ProfileCategory::L2Switch);
        assert_eq!(ms.templates, vec![TEMPLATE_STANDARD_SNMP]);
    }
}
