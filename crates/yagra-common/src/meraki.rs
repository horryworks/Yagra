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
}

impl MerakiListing {
    /// Every listing.
    pub const ALL: [MerakiListing; 5] = [
        MerakiListing::Availabilities,
        MerakiListing::UplinksLossAndLatency,
        MerakiListing::ApplianceUplinkStatuses,
        MerakiListing::ApplianceVpnStatuses,
        MerakiListing::ApplianceUplinksUsage,
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
        }
    }

    /// Read a token back. `None` for one this build does not know.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == s)
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
    /// MX WAN uplink usage — sent and received per uplink over the tier's interval (heavier, low
    /// cadence). The switches and access points have no reading here (ADR-164 決定 23).
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
    fn a_listing_token_is_its_serde_tag_and_reads_back() {
        for l in MerakiListing::ALL {
            assert_eq!(
                serde_json::to_value(l).unwrap(),
                serde_json::Value::String(l.as_str().to_owned()),
                "{l:?}"
            );
            assert_eq!(MerakiListing::from_token(l.as_str()), Some(l));
        }
        assert_eq!(MerakiListing::from_token("switch_port_statuses"), None);
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
