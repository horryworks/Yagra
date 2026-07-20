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
//! carries the org API key, can never be redirected off-host and leak the key.
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
/// Stable TSDB metric: per-uplink status (`active`=2, `ready`=1, otherwise 0).
pub const METRIC_MERAKI_UPLINK_STATUS: &str = "meraki_uplink_status";
/// Stable TSDB metric: number of clients seen on the device/network (gauge).
pub const METRIC_MERAKI_CLIENT_COUNT: &str = "meraki_client_count";
/// Stable TSDB metric: traffic sent over the reported window, kilobytes.
///
/// **ADR-012 exception:** Meraki returns *pre-aggregated windowed usage*, not a monotonic counter,
/// so this is stored as a **gauge** — never hand-rolled counter deltas or a query-time `rate()`.
pub const METRIC_MERAKI_USAGE_SENT_KB: &str = "meraki_usage_sent_kb";
/// Stable TSDB metric: traffic received over the reported window, kilobytes (gauge — see above).
pub const METRIC_MERAKI_USAGE_RECV_KB: &str = "meraki_usage_recv_kb";

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
    /// Device & network traffic usage and client counts (heavier, low cadence).
    Traffic,
    /// Inventory reconciliation: networks + devices (very low cadence).
    Inventory,
}

impl MerakiTier {
    /// Every tier, in cadence order (most frequent → least).
    pub const ALL: [MerakiTier; 4] = [
        MerakiTier::Availability,
        MerakiTier::Uplink,
        MerakiTier::Traffic,
        MerakiTier::Inventory,
    ];

    /// The stable snake_case token stored in the DB / sent over the bus.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MerakiTier::Availability => "availability",
            MerakiTier::Uplink => "uplink",
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

/// Encode a Meraki uplink status string as the `meraki_uplink_status` gauge value.
#[must_use]
pub fn uplink_status_value(status: &str) -> f64 {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => 2.0,
        "ready" => 1.0,
        // connecting / not connected / failed / unknown
        _ => 0.0,
    }
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
/// Meraki owns `meraki.com` / `meraki.cn` / `gov-meraki.com`, so a third party can't register a
/// matching subdomain.
#[must_use]
pub fn is_meraki_api_host(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    const EXACT: [&str; 3] = ["api.meraki.com", "api.meraki.cn", "api.gov-meraki.com"];
    EXACT.contains(&h.as_str())
        || h.ends_with(".meraki.com")
        || h.ends_with(".meraki.cn")
        || h.ends_with(".gov-meraki.com")
}

/// A node's Meraki-device binding (1:1 with the node). No secrets: the org API key is held by the
/// `meraki_orgs` row's credential and inlined by core at dispatch time (ADR-018/020).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    fn uplink_status_encoding() {
        assert_eq!(uplink_status_value("active"), 2.0);
        assert_eq!(uplink_status_value("Ready"), 1.0);
        assert_eq!(uplink_status_value("failed"), 0.0);
        assert_eq!(uplink_status_value("not connected"), 0.0);
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
        assert!(is_meraki_api_host("api.gov-meraki.com"));
        assert!(is_meraki_api_host("n123.meraki.com")); // shard host
                                                        // Refused: the key-exfiltration surface.
        assert!(!is_meraki_api_host("evil.com"));
        assert!(!is_meraki_api_host("api.meraki.com.evil.com"));
        assert!(!is_meraki_api_host("notmeraki.com"));
        assert!(!is_meraki_api_host("meraki.com.attacker.net"));
    }
}
