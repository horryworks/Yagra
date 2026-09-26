// SPDX-License-Identifier: AGPL-3.0-only
//! Cisco Meraki Dashboard API monitoring: configuration and metric vocabulary.
//!
//! A Meraki device is modelled as an ordinary node carrying its real role category (MX→Firewall,
//! MS→L2Switch, MR→WirelessAp) plus a single [`MerakiDeviceConfig`] (1:1 with the node). What makes
//! a node "Meraki" is the side-table row, not the profile category — so it reuses the whole
//! monitoring spine (thresholds, alerting, dashboards) exactly like a URL monitor does.
//!
//! Unlike SNMP/ICMP, Meraki is polled per **organization** (the Dashboard API is org-scoped and
//! bulk): one paged call returns data for many devices. Collection is grouped into [`MerakiTier`]s
//! with independent cadences so the precious per-org rate budget is spent deliberately.
//!
//! The integration is strictly **READ-ONLY**: the poller only issues HTTP GET. Every request host
//! is checked against [`is_meraki_api_host`] (the analog of [`crate::is_ssrf_blocked`]) — on the
//! initial URL and on every pagination `Link: rel=next` — so an authenticated request, which
//! carries the org API key, can never be redirected off-host and leak the key. The only way a
//! request goes anywhere else is a lab build's wire origin (`yagra_transport::MerakiWireOrigin`,
//! ADR-166), which whoever runs that box sets and a release build cannot.
//!
//! Secrets never live in these types: the org's API key is referenced by the `meraki_orgs` row's
//! credential and inlined by core over the bus at dispatch time (ADR-018/020).

use crate::profile::ProfileCategory;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable TSDB metric: `1` = the Dashboard reports the device online/reachable, `0` = down. One
/// metric expresses liveness so a single threshold (`meraki_device_up` below 0.5) covers it,
/// mirroring `http_up`.
pub const METRIC_MERAKI_DEVICE_UP: &str = "meraki_device_up";
/// Stable TSDB metric: seconds since the device was last seen by the Dashboard (diagnostic gauge).
pub const METRIC_MERAKI_LAST_SEEN_SECS: &str = "meraki_device_last_seen_secs";
/// Stable TSDB metric: per-uplink loss percentage (0–100), keyed by the synthetic uplink ifindex.
pub const METRIC_MERAKI_UPLINK_LOSS_PCT: &str = "meraki_uplink_loss_pct";
/// Stable TSDB metric: per-uplink average latency in milliseconds.
pub const METRIC_MERAKI_UPLINK_LATENCY_MS: &str = "meraki_uplink_latency_ms";
/// Stable TSDB metric: per-uplink status — `active` 2, `ready` 1, `not connected` / `connecting` /
/// a word this build does not know 0, **`failed` −1** ([`MerakiUplinkStatus::gauge`]). Before ADR-164
/// 決定 24 a failed uplink was stored as 0 as well, so older history cannot tell the two apart.
pub const METRIC_MERAKI_UPLINK_STATUS: &str = "meraki_uplink_status";
/// Stable TSDB metric: per-uplink `1` when the Dashboard reports the uplink `failed`, else `0` — what
/// the seeded Meraki rule alerts on (ADR-164 決定 24). Emitted for every uplink status row, healthy
/// ones included, so an open alert always has a reading to close on.
pub const METRIC_MERAKI_UPLINK_FAILED: &str = "meraki_uplink_failed";
/// Stable TSDB metric: per-uplink average send rate over the traffic collect's window, bits per
/// second — an MX appliance's WAN uplinks only (`appliance/uplinks/usage/byNetwork`, ADR-164 決定
/// 23).
///
/// **ADR-012 exception:** Meraki returns *pre-aggregated windowed usage* (bytes over the window),
/// not a monotonic counter, so this is stored as a **gauge** — never hand-rolled counter deltas or a
/// query-time `rate()`. It is stored as a rate rather than as the window's bytes so that changing
/// the traffic tier's interval does not change what the number means.
pub const METRIC_MERAKI_UPLINK_SENT_BPS: &str = "meraki_uplink_sent_bps";
/// Stable TSDB metric: per-uplink average receive rate over the window, bits per second (see above).
pub const METRIC_MERAKI_UPLINK_RECV_BPS: &str = "meraki_uplink_recv_bps";
/// Stable TSDB metric (ADR-164 決定 25): how many of an MX's Auto VPN **hub** peers it reaches. Node
/// level, on the MX the VPN row names, and only while that MX is up — a down device's row is stale.
/// A peer hub that is itself down is not counted either way: it raises its own alert, and counting it
/// would put every spoke of a dead hub into warning at once.
pub const METRIC_MERAKI_VPN_HUBS_REACHABLE: &str = "meraki_vpn_hubs_reachable";
/// Stable TSDB metric: how many of an MX's counted Auto VPN hub peers it does NOT reach (see above).
pub const METRIC_MERAKI_VPN_HUBS_UNREACHABLE: &str = "meraki_vpn_hubs_unreachable";
/// Stable TSDB metric: the unreachable share of an MX's counted hub peers, 0–100. What the seeded rule
/// reads: any unreachable hub is a warning (redundancy lost), all of them critical — one alert that
/// escalates. Emitted only when at least one hub was counted.
pub const METRIC_MERAKI_VPN_HUBS_UNREACHABLE_PCT: &str = "meraki_vpn_hubs_unreachable_pct";
/// Stable TSDB metric: on an Auto VPN **hub**, how many of its spokes it does not reach, counted like
/// the hubs above. Display only — a spoke that is down alerts for itself.
pub const METRIC_MERAKI_VPN_SPOKES_UNREACHABLE: &str = "meraki_vpn_spokes_unreachable";

/// The per-uplink metrics — the ones whose row key is a synthetic uplink index (WAN1 = 1, WAN2 = 2,
/// cellular = 3) and whose rows are named after the uplink (ADR-164 決定 24).
///
/// 🚨 **Not every Meraki sample with a row key is an uplink's.** A switch port's samples carry the
/// port's own number (ADR-167), so naming "every sample with a row key" after the uplinks called
/// ports 1–3 of every switch WAN1, WAN2 and cellular. Whatever names uplink rows reads this list.
pub const MERAKI_UPLINK_ROW_METRICS: [&str; 6] = [
    METRIC_MERAKI_UPLINK_LOSS_PCT,
    METRIC_MERAKI_UPLINK_LATENCY_MS,
    METRIC_MERAKI_UPLINK_STATUS,
    METRIC_MERAKI_UPLINK_FAILED,
    METRIC_MERAKI_UPLINK_SENT_BPS,
    METRIC_MERAKI_UPLINK_RECV_BPS,
];

/// Stable TSDB metric (ADR-167 決定 6): a Meraki switch port's average **receive** rate over one
/// five-minute bucket of `switch/ports/usage/history/byDevice/byInterval`, bits per second — the
/// Dashboard's `downstream`. Measured on a real organization: an uplink port's downstream was about
/// twice its upstream, and every access port's the other way round.
///
/// **ADR-012 exception, like the uplink rates:** Meraki reports a per-bucket average and never a
/// monotonic counter, so this is a gauge, and the interface reads fold it into the same bits/s as
/// `rate(if_hc_in_octets) * 8` with PromQL's `or` (`yagra-core`'s `store.rs`). ⚠️ The bucket read
/// is one that ended at least twelve minutes before the collect — the Dashboard fills a bucket five
/// to eleven minutes after it ends — and the sample is stored at the collect's time, so the line
/// runs twelve to seventeen minutes behind the port.
pub const METRIC_MERAKI_PORT_IN_BPS: &str = "meraki_port_in_bps";
/// Stable TSDB metric: a Meraki switch port's average **send** rate (the Dashboard's `upstream`),
/// bits per second. See [`METRIC_MERAKI_PORT_IN_BPS`].
pub const METRIC_MERAKI_PORT_OUT_BPS: &str = "meraki_port_out_bps";

/// The Meraki metrics that publish **one series per switch port**, keyed by the port's ifindex
/// ([`switch_port_ifindex`]). No collection item carries them — the Dashboard is not walked — so the
/// set of per-interface metric names (`yagra-core`'s `per_interface_metric_names`) adds this list
/// rather than learning it from a template (ADR-167 決定 8). A port's status and speed use the SNMP
/// names (`if_oper_status`, `if_admin_status`, `if_high_speed`), which the built-in catalog already
/// declares.
pub const MERAKI_PORT_METRICS: [&str; 2] = [METRIC_MERAKI_PORT_IN_BPS, METRIC_MERAKI_PORT_OUT_BPS];

/// The radio metrics only a Meraki MR publishes, one series per radio slot (ADR-168 決定 2). The
/// other radio readings an MR sends — utilization, channel, transmit power — share their names with
/// the controller-walked access points, and the built-in radio template already declares those; this
/// one no template can declare, for the reason [`MERAKI_PORT_METRICS`] gives.
pub const MERAKI_RADIO_METRICS: [&str; 1] = [crate::wlan::METRIC_WLAN_RADIO_NON_WIFI_UTIL_PCT];

/// Every metric the Meraki collect publishes **per interface** that no collection item declares:
/// a switch port's traffic ([`MERAKI_PORT_METRICS`]) and an access point's non-Wi-Fi utilization
/// ([`MERAKI_RADIO_METRICS`]). The one list both readers take — the per-interface metric set the
/// alert engine judges by, and the API's dimension of a series with no item behind it — so a third
/// list cannot be added to one of them and forgotten by the other.
pub fn meraki_interface_metrics() -> impl Iterator<Item = &'static str> {
    MERAKI_PORT_METRICS.into_iter().chain(MERAKI_RADIO_METRICS)
}

/// One Dashboard read a collect makes (ADR-164 決定 25). A tier may make several — the uplink tier
/// reads loss and latency, the uplinks' statuses and the Auto VPN statuses — and when one of them fails
/// while the others answer, this names which, on the organization's row and in the API.
///
/// The token travels as a plain string on the bus (`MerakiCollectReport.listing`) and is read back with
/// [`Self::from_token`], so a listing a newer poller names costs the label, never the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MerakiListing {
    /// `devices/availabilities` — the availability tier.
    Availabilities,
    /// `devices/uplinksLossAndLatency` — the uplink tier's first read.
    UplinksLossAndLatency,
    /// `appliance/uplink/statuses` — the uplink tier's second read.
    ApplianceUplinkStatuses,
    /// `appliance/vpn/statuses` — the uplink tier's third read (決定 25).
    ApplianceVpnStatuses,
    /// `appliance/uplinks/usage/byNetwork` — the traffic tier (決定 23).
    ApplianceUplinksUsage,
    /// `switch/ports/statuses/bySwitch` — the switch-port tier's first read (ADR-167).
    SwitchPortStatuses,
    /// `switch/ports/usage/history/byDevice/byInterval` — the switch-port tier's second read.
    SwitchPortUsage,
    /// `switch/ports/bySwitch` — the ports' configured names, read once an hour (ADR-167 決定 1).
    SwitchPortConfig,
    /// `switch/ports/topology/discovery/byDevice` — each port's LLDP/CDP neighbours, read at the
    /// deployment's neighbour interval (ADR-181).
    SwitchPortTopology,
    /// `wireless/clients/overview/byDevice` — the wireless tier's first read: each access point's
    /// clients online (ADR-168).
    WirelessClients,
    /// `wireless/devices/channelUtilization/byDevice` — the wireless tier's second read: each radio's
    /// channel utilization over the last five minutes.
    WirelessChannelUtilization,
    /// `wireless/ssids/statuses/byDevice` — the SSIDs and radio settings, read every twenty minutes
    /// (ADR-168 決定 1).
    WirelessSsidStatuses,
}

impl MerakiListing {
    /// Every listing.
    pub const ALL: [MerakiListing; 12] = [
        MerakiListing::Availabilities,
        MerakiListing::UplinksLossAndLatency,
        MerakiListing::ApplianceUplinkStatuses,
        MerakiListing::ApplianceVpnStatuses,
        MerakiListing::ApplianceUplinksUsage,
        MerakiListing::SwitchPortStatuses,
        MerakiListing::SwitchPortUsage,
        MerakiListing::SwitchPortConfig,
        MerakiListing::SwitchPortTopology,
        MerakiListing::WirelessClients,
        MerakiListing::WirelessChannelUtilization,
        MerakiListing::WirelessSsidStatuses,
    ];

    /// The token stored and sent — the serde tag.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MerakiListing::Availabilities => "availabilities",
            MerakiListing::UplinksLossAndLatency => "uplinks_loss_and_latency",
            MerakiListing::ApplianceUplinkStatuses => "appliance_uplink_statuses",
            MerakiListing::ApplianceVpnStatuses => "appliance_vpn_statuses",
            MerakiListing::ApplianceUplinksUsage => "appliance_uplinks_usage",
            MerakiListing::SwitchPortStatuses => "switch_port_statuses",
            MerakiListing::SwitchPortUsage => "switch_port_usage",
            MerakiListing::SwitchPortConfig => "switch_port_config",
            MerakiListing::SwitchPortTopology => "switch_port_topology",
            MerakiListing::WirelessClients => "wireless_clients",
            MerakiListing::WirelessChannelUtilization => "wireless_channel_utilization",
            MerakiListing::WirelessSsidStatuses => "wireless_ssid_statuses",
        }
    }

    /// Read a token back. `None` for one this build does not know.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == s)
    }
}

/// The role an MX is **configured** to hold in its warm-spare pair (ADR-164 決定 26), from
/// `appliance/uplink/statuses`' `highAvailability.role` while `highAvailability.enabled` is true.
///
/// ⚠️ Configured, not current: measured on a real organization, a primary that was down still said
/// `primary` while its spare — still saying `spare` — carried the traffic. So "running on the spare"
/// is worked out from the two devices' liveness, never read from this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MerakiHaRole {
    /// The pair's primary.
    Primary,
    /// The pair's spare (Meraki's word for the standby).
    Spare,
}

impl MerakiHaRole {
    /// Every role.
    pub const ALL: [MerakiHaRole; 2] = [MerakiHaRole::Primary, MerakiHaRole::Spare];

    /// The token stored in `meraki_inventory.ha_role` — the serde tag.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MerakiHaRole::Primary => "primary",
            MerakiHaRole::Spare => "spare",
        }
    }

    /// Read a token back (the Dashboard's word or the stored one). `None` for anything else.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|r| r.as_str() == s.trim().to_ascii_lowercase())
    }
}

/// A Meraki collection tier: a group of Dashboard endpoints polled together on one cadence.
///
/// Each `(org, enabled tier)` is one org-scoped collector job. Tiers exist because Meraki data has
/// very different freshness needs (device up ~minutes, traffic ~tens of minutes, inventory ~hours)
/// and the per-org API budget is shared with the customer's own tooling, so it is spent sparingly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MerakiTier {
    /// Device availability / reachability (cheap, frequent).
    Availability,
    /// WAN uplink loss / latency / status.
    Uplink,
    /// Every access point's clients, each radio's channel utilization and — every twenty minutes —
    /// the SSIDs it broadcasts and its radios' channel and power (ADR-168). The MR access points
    /// only, read organization-wide and joined by serial. Observational: nothing here says whether
    /// an access point is up.
    Wireless,
    /// Every switch port's status, speed and traffic (ADR-167) — the MS switches only, read
    /// organization-wide and joined by serial. Observational: a port's readings say nothing about
    /// whether its switch is up.
    SwitchPorts,
    /// MX WAN uplink usage — sent and received per uplink over the tier's interval (heavier, low
    /// cadence). The switches and access points have no reading here (ADR-164 決定 23).
    Traffic,
    /// Inventory reconciliation: networks + devices (very low cadence).
    Inventory,
}

impl MerakiTier {
    /// Every tier, in cadence order (most frequent → least).
    pub const ALL: [MerakiTier; 6] = [
        MerakiTier::Availability,
        MerakiTier::Uplink,
        MerakiTier::Wireless,
        MerakiTier::SwitchPorts,
        MerakiTier::Traffic,
        MerakiTier::Inventory,
    ];

    /// The stable snake_case token stored in the DB / sent over the bus.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MerakiTier::Availability => "availability",
            MerakiTier::Uplink => "uplink",
            MerakiTier::Wireless => "wireless",
            MerakiTier::SwitchPorts => "switch_ports",
            MerakiTier::Traffic => "traffic",
            MerakiTier::Inventory => "inventory",
        }
    }

    /// Parse a stored/operator token back into a tier.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

/// The fixed, low-cardinality synthetic ifindex for a Meraki uplink interface name.
///
/// Meraki appliances have at most a few uplinks (WAN1/WAN2/cellular), so a stable small integer
/// keeps the `(node, ifindex)` series count bounded (thin-label model, ADR-011). Returns `None` for
/// an unrecognised uplink — the poller then skips it rather than inventing an unbounded label.
#[must_use]
pub fn uplink_ifindex(name: &str) -> Option<u32> {
    match name.trim().to_ascii_lowercase().as_str() {
        "wan1" => Some(1),
        "wan2" => Some(2),
        "cellular" | "wan3" => Some(3),
        _ => None,
    }
}

/// The canonical display name for a synthetic uplink ifindex, stored in the `interfaces` inventory
/// so the UI can label the series (the metric itself stays thin-labelled by `ifindex`).
#[must_use]
pub fn uplink_name(ifindex: u32) -> Option<&'static str> {
    match ifindex {
        1 => Some("WAN1"),
        2 => Some("WAN2"),
        3 => Some("cellular"),
        _ => None,
    }
}

/// A WAN uplink's status word from `appliance/uplink/statuses` (ADR-164 決定 24).
///
/// `failed` and `not connected` used to be one number. Measured on a real organization (2026-09-22):
/// on the online appliances every `not connected` uplink (284) had no address at all — a port with no
/// line behind it — and `failed` was 2. A rule on the shared 0 would have raised ~300 false alarms
/// and buried the two real ones. ⚠️ What an in-use uplink whose cable is pulled reports was never
/// observed (no uplink changed state in a recorded hour), so an unplugged line may read as
/// `NotConnected` and not alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MerakiUplinkStatus {
    /// Carrying traffic.
    Active,
    /// Up and standing by.
    Ready,
    /// Coming up.
    Connecting,
    /// No link — usually a port with no line behind it.
    NotConnected,
    /// The Dashboard says the uplink failed.
    Failed,
    /// A word this build does not know.
    Other,
}

impl MerakiUplinkStatus {
    /// Read the Dashboard's word (case and surrounding space ignored).
    #[must_use]
    pub fn from_word(word: &str) -> Self {
        match word.trim().to_ascii_lowercase().as_str() {
            "active" => Self::Active,
            "ready" => Self::Ready,
            "connecting" => Self::Connecting,
            "not connected" => Self::NotConnected,
            "failed" => Self::Failed,
            _ => Self::Other,
        }
    }

    /// The `meraki_uplink_status` value: better is higher, and only `failed` is below zero — so an
    /// operator's `below 0.5` rule still fires on both 0 and −1, exactly as before.
    #[must_use]
    pub fn gauge(self) -> f64 {
        match self {
            Self::Active => 2.0,
            Self::Ready => 1.0,
            Self::Connecting | Self::NotConnected | Self::Other => 0.0,
            Self::Failed => -1.0,
        }
    }

    /// Whether this is the one status the seeded rule alerts on.
    #[must_use]
    pub fn failed(self) -> bool {
        match self {
            Self::Failed => true,
            Self::Active | Self::Ready | Self::Connecting | Self::NotConnected | Self::Other => {
                false
            }
        }
    }
}

/// The ifindex a Meraki switch port is stored under (ADR-167 決定 4) — the row key of its series,
/// its `interfaces` row, its threshold overrides and its alert history, so **a value handed out here
/// can never be changed**.
///
/// * A plain decimal port id with no leading zero (`"1"` … `"54"` — every port of the organization
///   the design was measured on) is its own number, so port 7 is ifindex 7, as on an SNMP switch.
/// * Anything else — a module port such as `1_MA-MOD-8X10G_1`, a stack member's `"2_10"`, `"007"`,
///   `"0"`, or a number too large to be one — is folded with 32-bit FNV-1a into `[2^30, 2^31 − 1)`:
///   above every plain number a switch has, and below `i32::MAX`, so the `interfaces` table's
///   INTEGER column holds it.
///
/// 🚨 FNV-1a and not `std::hash::DefaultHasher`: the standard hasher's output may change between
/// Rust releases, which would move every folded port to a new row on the day the toolchain is
/// bumped. The test pins the values.
#[must_use]
pub fn switch_port_ifindex(port_id: &str) -> u32 {
    /// The first folded value; every plain port number stays below it.
    const FOLD_BASE: u32 = 1 << 30;
    let id = port_id.trim();
    if !id.is_empty() && !id.starts_with('0') && id.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(n) = id.parse::<u32>() {
            if n < FOLD_BASE {
                return n;
            }
        }
    }
    let mut hash: u32 = 0x811c_9dc5;
    for b in id.bytes() {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    // 2^30 − 1 slots, so `i32::MAX` itself is never produced.
    FOLD_BASE + hash % (FOLD_BASE - 1)
}

/// A switch port's link-state word from `switch/ports/statuses/bySwitch` (ADR-167 決定 5), as the
/// value `if_oper_status` takes on an SNMP switch: `Connected` is up (1), `Disconnected` down (2).
/// `None` for a word this build does not know — it says nothing rather than something invented.
#[must_use]
pub fn switch_port_oper_status(word: &str) -> Option<f64> {
    match word.trim().to_ascii_lowercase().as_str() {
        "connected" => Some(1.0),
        "disconnected" => Some(2.0),
        _ => None,
    }
}

/// A switch port's line rate in bits per second, from the Dashboard's speed word (`"1 Gbps"`,
/// `"100 Mbps"`, `"2.5 Gbps"`). `None` for `""` — a port with no link; measured on 19,357 of 25,092
/// ports — and for anything that is not a positive number and a unit this build knows.
#[must_use]
pub fn switch_port_speed_bps(word: &str) -> Option<i64> {
    let (number, unit) = word.trim().split_once(' ')?;
    let number: f64 = number.trim().parse().ok()?;
    let scale = match unit.trim().to_ascii_lowercase().as_str() {
        "kbps" => 1e3,
        "mbps" => 1e6,
        "gbps" => 1e9,
        "tbps" => 1e12,
        _ => return None,
    };
    let bps = (number * scale).round();
    // The guard keeps NaN, zero and the negative out; a line rate is far below `i64::MAX`, and `as`
    // saturates rather than wrapping if one ever were not.
    #[allow(clippy::cast_possible_truncation)]
    (bps.is_finite() && bps >= 1.0).then_some(bps as i64)
}

/// Map a Meraki `productType` to the Yagra role category the imported node should carry. Unknown
/// product types fall back to `GenericSnmp` (still a valid node; the operator can re-profile).
#[must_use]
pub fn category_for_product_type(product_type: &str) -> ProfileCategory {
    match product_type.trim().to_ascii_lowercase().as_str() {
        "appliance" => ProfileCategory::Firewall, // MX security appliance
        "switch" => ProfileCategory::L2Switch,    // MS
        "wireless" => ProfileCategory::WirelessAp, // MR
        "cellulargateway" | "cellular_gateway" => ProfileCategory::Router, // MG
        _ => ProfileCategory::GenericSnmp,        // MV cameras, sensors, etc.
    }
}

/// The built-in Meraki-API profile name for a `productType`, if one exists (the MX/MS/MR profiles
/// seeded in [`crate::builtin_profiles`]). `None` ⇒ core falls back to a category-derived profile.
#[must_use]
pub fn api_profile_name_for_product_type(product_type: &str) -> Option<&'static str> {
    match product_type.trim().to_ascii_lowercase().as_str() {
        "appliance" => Some(PROFILE_MERAKI_MX_API),
        "switch" => Some(PROFILE_MERAKI_MS_API),
        "wireless" => Some(PROFILE_MERAKI_MR_API),
        _ => None,
    }
}

/// Built-in profile name: Meraki MX security appliance monitored via the Dashboard API.
pub const PROFILE_MERAKI_MX_API: &str = "Cisco Meraki MX (API)";
/// Built-in profile name: Meraki MS switch monitored via the Dashboard API.
pub const PROFILE_MERAKI_MS_API: &str = "Cisco Meraki MS (API)";
/// Built-in profile name: Meraki MR wireless AP monitored via the Dashboard API.
pub const PROFILE_MERAKI_MR_API: &str = "Cisco Meraki MR (API)";

/// Whether `host` is an allowed Meraki Dashboard API host.
///
/// The integration is read-only, but every request still carries the org API key, so we refuse any
/// host not on this list — on the initial URL **and** on every pagination `Link: rel=next` — so a
/// redirect/next-link can never exfiltrate the key to a non-Meraki host. Suffix matches are safe:
/// Meraki owns `meraki.com` / `meraki.ca` / `meraki.cn` / `gov-meraki.com`, so a third party can't
/// register a matching subdomain. A lab build's wire origin (ADR-166) is applied only *after* this
/// check has passed, and changes where the request is physically sent — never what passes here.
///
/// **Every region the WebUI offers has to be on this list**, and for a while one was not: the
/// region picker listed Canada (`api.meraki.ca`) while this function refused it, so choosing it
/// answered `400 invalid_base_url` every time (ADR-164). The picker's list is
/// `web/src/pages/integrations/merakiRegions.ts`, and a test beside the API's base-url validator
/// reads that file and runs each URL through here. ⚠️ The Canada host comes from Meraki's
/// published regional base URIs; no live Canadian organization has been pointed at it.
#[must_use]
pub fn is_meraki_api_host(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    const EXACT: [&str; 4] = [
        "api.meraki.com",
        "api.meraki.ca",
        "api.meraki.cn",
        "api.gov-meraki.com",
    ];
    EXACT.contains(&h.as_str())
        || h.ends_with(".meraki.com")
        || h.ends_with(".meraki.ca")
        || h.ends_with(".meraki.cn")
        || h.ends_with(".gov-meraki.com")
}

/// A node's Meraki-device binding (1:1 with the node). No secrets: the org API key is held by the
/// `meraki_orgs` row's credential and inlined by core at dispatch time (ADR-018/020).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct MerakiDeviceConfig {
    /// Internal handle of the owning `meraki_orgs` row.
    pub org_uuid: Uuid,
    /// The Meraki organizationId (the API path segment) — denormalised for display.
    pub org_id: String,
    /// The device serial — the join key returned by the org-bulk endpoints.
    pub serial: String,
    /// The Meraki networkId the device belongs to.
    pub network_id: String,
    /// Meraki productType (appliance/switch/wireless/…).
    pub product_type: String,
    /// Device model (e.g. "MX67") — display only.
    #[serde(default)]
    pub model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_token_roundtrips() {
        for t in MerakiTier::ALL {
            assert_eq!(MerakiTier::from_token(t.as_str()), Some(t));
        }
        assert_eq!(MerakiTier::from_token("nonsense"), None);
    }

    /// ADR-167 決定 4. These values are row keys in the TSDB, the `interfaces` table and the alert
    /// history, so they are pinned: a change here moves every folded port to a new row.
    #[test]
    fn a_switch_port_id_is_its_own_number_or_a_pinned_folded_one() {
        for (id, want) in [("1", 1), ("7", 7), ("54", 54), (" 12 ", 12), ("11", 11)] {
            assert_eq!(switch_port_ifindex(id), want, "{id:?}");
        }
        for (id, want) in [
            ("1_MA-MOD-8X10G_1", 2_092_357_084),
            ("007", 1_657_484_817),
            ("0", 1_963_763_887),
            ("2_10", 1_546_608_473),
            ("1_1", 1_886_141_273),
            // 2^30: a plain number, but past the folded range's floor.
            ("1073741824", 1_900_538_227),
            ("", 1_092_394_439),
        ] {
            let got = switch_port_ifindex(id);
            assert_eq!(got, want, "{id:?}");
            assert!(
                ((1u32 << 30)..(i32::MAX as u32)).contains(&got),
                "{id:?} folded to {got}, outside [2^30, 2^31 - 1)"
            );
        }
    }

    #[test]
    fn a_switch_port_status_and_speed_read_only_the_words_they_know() {
        assert_eq!(switch_port_oper_status("Connected"), Some(1.0));
        assert_eq!(switch_port_oper_status(" disconnected "), Some(2.0));
        assert_eq!(switch_port_oper_status("Disabled"), None);
        assert_eq!(switch_port_oper_status(""), None);

        for (word, want) in [
            ("10 Mbps", Some(10_000_000)),
            ("100 Mbps", Some(100_000_000)),
            ("1 Gbps", Some(1_000_000_000)),
            ("2.5 Gbps", Some(2_500_000_000)),
            ("10 Gbps", Some(10_000_000_000)),
            ("20 Gbps", Some(20_000_000_000)),
            ("", None),
            ("auto", None),
            ("1 Parsec", None),
            ("0 Mbps", None),
            ("-1 Gbps", None),
        ] {
            assert_eq!(switch_port_speed_bps(word), want, "{word:?}");
        }
    }

    #[test]
    fn only_the_uplink_metrics_are_named_after_uplinks() {
        for m in MERAKI_PORT_METRICS {
            assert!(!MERAKI_UPLINK_ROW_METRICS.contains(&m), "{m}");
        }
        assert!(MERAKI_UPLINK_ROW_METRICS.contains(&METRIC_MERAKI_UPLINK_SENT_BPS));
    }

    #[test]
    fn uplink_ifindex_is_bounded_and_named() {
        assert_eq!(uplink_ifindex("wan1"), Some(1));
        assert_eq!(uplink_ifindex("WAN2"), Some(2));
        assert_eq!(uplink_ifindex("cellular"), Some(3));
        assert_eq!(uplink_ifindex("eth9"), None);
        assert_eq!(uplink_name(1), Some("WAN1"));
        assert_eq!(uplink_name(99), None);
    }

    #[test]
    fn a_ha_role_token_is_its_serde_tag_and_reads_the_dashboards_word() {
        for r in MerakiHaRole::ALL {
            assert_eq!(
                serde_json::to_value(r).unwrap(),
                serde_json::Value::String(r.as_str().to_owned())
            );
            assert_eq!(MerakiHaRole::from_token(r.as_str()), Some(r));
        }
        assert_eq!(
            MerakiHaRole::from_token(" Primary "),
            Some(MerakiHaRole::Primary)
        );
        assert_eq!(MerakiHaRole::from_token("standby"), None);
    }

    #[test]
    fn a_listing_token_is_its_serde_tag_and_reads_back() {
        for l in MerakiListing::ALL {
            assert_eq!(
                serde_json::to_value(l).unwrap(),
                serde_json::Value::String(l.as_str().to_owned()),
                "{l:?}"
            );
            assert_eq!(MerakiListing::from_token(l.as_str()), Some(l));
        }
        assert_eq!(MerakiListing::from_token("switch_port_errors"), None);
    }

    #[test]
    fn uplink_status_words_become_the_gauge_and_the_failed_flag() {
        // (word, gauge, failed)
        for (word, gauge, failed) in [
            ("active", 2.0, false),
            ("Ready", 1.0, false),
            ("connecting", 0.0, false),
            ("not connected", 0.0, false),
            (" Not Connected ", 0.0, false),
            ("failed", -1.0, true),
            ("FAILED", -1.0, true),
            ("something new", 0.0, false),
            ("", 0.0, false),
        ] {
            let s = MerakiUplinkStatus::from_word(word);
            assert_eq!(s.gauge(), gauge, "{word:?}");
            assert_eq!(s.failed(), failed, "{word:?}");
        }
        // The one distinction this exists for (ADR-164 決定 24).
        assert_ne!(
            MerakiUplinkStatus::from_word("failed").gauge(),
            MerakiUplinkStatus::from_word("not connected").gauge()
        );
    }

    #[test]
    fn product_type_maps_to_role_category() {
        assert_eq!(
            category_for_product_type("appliance"),
            ProfileCategory::Firewall
        );
        assert_eq!(
            category_for_product_type("switch"),
            ProfileCategory::L2Switch
        );
        assert_eq!(
            category_for_product_type("wireless"),
            ProfileCategory::WirelessAp
        );
        assert_eq!(
            category_for_product_type("sensor"),
            ProfileCategory::GenericSnmp
        );
    }

    #[test]
    fn allow_lists_only_meraki_hosts() {
        // Allowed: canonical + regional shards.
        assert!(is_meraki_api_host("api.meraki.com"));
        assert!(is_meraki_api_host("API.Meraki.com")); // case-insensitive
        assert!(is_meraki_api_host("api.meraki.cn"));
        assert!(is_meraki_api_host("api.meraki.ca")); // Canada — offered by the WebUI (ADR-164)
        assert!(is_meraki_api_host("api.gov-meraki.com"));
        assert!(is_meraki_api_host("n123.meraki.com")); // shard host
                                                        // Refused: the key-exfiltration surface.
        assert!(!is_meraki_api_host("evil.com"));
        assert!(!is_meraki_api_host("api.meraki.com.evil.com"));
        assert!(!is_meraki_api_host("notmeraki.com"));
        assert!(!is_meraki_api_host("meraki.com.attacker.net"));
        assert!(!is_meraki_api_host("api.meraki.ca.attacker.net"));
        assert!(!is_meraki_api_host("notmeraki.ca"));
    }
}
