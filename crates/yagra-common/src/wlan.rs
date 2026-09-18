// SPDX-License-Identifier: AGPL-3.0-only
//! Wireless LAN controllers and the access points behind them (ADR-064).
//!
//! One controller walk answers for every AP it manages, so the vocabulary here is shared by three
//! programs that must agree without talking: the poller that walks the controller, core that stores
//! the inventory and decides which controller owns an AP, and — later — the Meraki adapter, whose
//! "controller" is a Dashboard network rather than an SNMP agent (ADR-064 改訂 R1).
//!
//! Three decisions shape the module, and each is written down once, here:
//!
//! 1. **An AP is identified by its MAC and nothing else** ([`ap_id`], ADR-064 決定 8b). A controller
//!    that fails over to its HA peer reports the same AP, and that AP must stay the same node —
//!    otherwise its history, alerts and thresholds split in two at every switchover.
//! 2. **A controller's statement about an AP is one of three things** ([`WlanApState`]). Vendors
//!    have a dozen run states; what the rest of the system needs is whether *this* controller is
//!    serving the AP, standing by for it, or reports it as not working. An HA standby answers
//!    `Backup` for every AP its peer serves, **with CPU, memory and radio values of 0** — measured
//!    on the PoC's AC6508 pair — so a `Backup` observation must never become a sample.
//! 3. **The inventory crosses the bus bounded** ([`MAX_APS_PER_CONTROLLER_HARD`]). Past the cap the
//!    list is truncated and says where, rather than growing a message toward NATS's payload limit.
//!
//! Device-supplied strings (names, serials, models) are untrusted: they are cleaned with the same
//! function the row-name walk uses ([`crate::row_names::sanitize_row_name`]) on the poller, and
//! again by core on receipt.

use crate::row_names::sanitize_row_name;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::IpAddr;
use uuid::Uuid;

/// Did the controller's AP walk get every column it asked for (1) or not (0) — the one sample a
/// controller result carries, and the only thing that shows the AP data has stopped arriving
/// (ADR-064 決定 9b: a walk that is not complete publishes no inventory at all).
pub const METRIC_WLAN_AP_WALK_COMPLETE: &str = "wlan_ap_walk_complete";

/// An AP node's liveness: 1 while the controller serving it reports it in service, 0 while the
/// controller answering for it reports it down (ADR-064). The liveness metric of
/// [`crate::NodeKind::WirelessAp`].
pub const METRIC_WLAN_AP_UP: &str = "wlan_ap_up";
/// Wireless clients online through an AP, as the controller serving it reports.
pub const METRIC_WLAN_AP_CLIENT_COUNT: &str = "wlan_ap_client_count";
/// An AP's CPU in use, percent, as its controller reports.
pub const METRIC_WLAN_AP_CPU_PCT: &str = "wlan_ap_cpu_pct";
/// An AP's memory in use, percent, as its controller reports.
pub const METRIC_WLAN_AP_MEM_PCT: &str = "wlan_ap_mem_pct";
/// An AP's operating temperature, °C, as its controller reports. Absent for an AP with no sensor.
///
/// ⚠️ **Most APs have no such sensor.** Measured on the PoC's AC6508: 36 of 38 answered the
/// `255` placeholder and only 2 a reading. The die temperature the same APs do report is
/// [`METRIC_WLAN_AP_CPU_TEMP_C`], and the two are **different sensors** — never fold one into the
/// other (ADR-064 増分 E).
pub const METRIC_WLAN_AP_TEMP_C: &str = "wlan_ap_temp_c";
/// An AP's CPU die temperature, °C, as its controller reports (`hwWlanApCpuTemperature`).
///
/// The reading most AirEngine APs actually have: 30 of the PoC's 38 answered 55–69 °C, and the 8
/// that answered the `255` placeholder were exactly the 8 that were down. Sibling of
/// [`METRIC_WLAN_AP_TEMP_C`], not a replacement for it.
pub const METRIC_WLAN_AP_CPU_TEMP_C: &str = "wlan_ap_cpu_temp_c";
/// An AP's power supply state as its controller reports it, as the MIB's own enumeration —
/// `1` normal, `2` insufficient, `3` limited, `4` invalid (`hwWlanAPPowerSupplyState`).
///
/// ⚠️ Published only for an AP the controller is serving, so `4` (what a down AP answers) never
/// reaches the TSDB — a down AP is said by `wlan_ap_up`, not by a power state.
/// ⚠️ **`2` and `3` have never been observed.** Every measured AP answered `1` or `4`, so the two
/// interesting values rest on the MIB's wording alone (ADR-064 増分 E).
pub const METRIC_WLAN_AP_POWER_STATE: &str = "wlan_ap_power_state";

/// How many SSIDs the controller is broadcasting — the one node-level number the SSID walk adds.
///
/// Published only when the walk read every column, because a partial row count is a wrong answer
/// rather than a missing one (the per-SSID rows are each independent and are published either way).
pub const METRIC_WLAN_CONTROLLER_SSID_COUNT: &str = "wlan_controller_ssid_count";
/// Did the controller's SSID walk get every column it asked for (1) or not (0).
pub const METRIC_WLAN_SSID_WALK_COMPLETE: &str = "wlan_ssid_walk_complete";

/// Clients online on one SSID, summed over the bands the controller answered for.
///
/// One series per SSID on the **controller** node, keyed by [`ssid_row_key`]; the SSID's name is
/// joined at read time out of `entity_row_names` and is never a series label (ADR-011/ADR-143).
pub const METRIC_WLAN_SSID_CLIENTS: &str = "wlan_ssid_clients";
/// Clients online on one SSID over 2.4 GHz.
pub const METRIC_WLAN_SSID_CLIENTS_2G4: &str = "wlan_ssid_clients_2g4";
/// Clients online on one SSID over 5 GHz.
pub const METRIC_WLAN_SSID_CLIENTS_5G: &str = "wlan_ssid_clients_5g";
/// Clients online on one SSID over 6 GHz.
pub const METRIC_WLAN_SSID_CLIENTS_6G: &str = "wlan_ssid_clients_6g";
/// How many access points are broadcasting one SSID.
pub const METRIC_WLAN_SSID_AP_COUNT: &str = "wlan_ssid_ap_count";
/// Bytes received on one SSID's air interface, as a raw counter (ADR-012).
pub const METRIC_WLAN_SSID_IN_OCTETS: &str = "wlan_ssid_in_octets";
/// Bytes sent on one SSID's air interface, as a raw counter (ADR-012).
pub const METRIC_WLAN_SSID_OUT_OCTETS: &str = "wlan_ssid_out_octets";

/// Every metric the SSID walk publishes per SSID, gauges first.
///
/// Read by the catalogue ledger and by the reader that decides a metric's dimension, so neither can
/// come to hold a different idea of which names are rows of this table.
pub const WLAN_SSID_ROW_METRICS: [&str; 7] = [
    METRIC_WLAN_SSID_CLIENTS,
    METRIC_WLAN_SSID_CLIENTS_2G4,
    METRIC_WLAN_SSID_CLIENTS_5G,
    METRIC_WLAN_SSID_CLIENTS_6G,
    METRIC_WLAN_SSID_AP_COUNT,
    METRIC_WLAN_SSID_IN_OCTETS,
    METRIC_WLAN_SSID_OUT_OCTETS,
];

/// The metrics a WLAN collection item names that are **one number for the controller**, not rows.
///
/// 🚨 `dimension_of_item` cannot answer from [`crate::CollectionKind::Wlan`] alone any more: the AP
/// and SSID templates both carry that kind, and one publishes a node-level flag while the other
/// publishes a row per SSID. This list is what tells them apart, and it is read by the API edge and
/// by the catalogue generator so the screen and `/mcp` cannot disagree about which it is.
pub const WLAN_NODE_LEVEL_METRICS: [&str; 3] = [
    METRIC_WLAN_AP_WALK_COMPLETE,
    METRIC_WLAN_SSID_WALK_COMPLETE,
    METRIC_WLAN_CONTROLLER_SSID_COUNT,
];

/// The row key one SSID's series carry, derived from its **name**.
///
/// FNV-1a over the cleaned name, never over the index's sub-identifiers. Two reasons, and the
/// second is the one that matters later: `fold_subids` is private to `yagra-transport`, and a
/// Huawei AC indexes this table by the SSID string while Cisco's equivalent indexes it by a number
/// — so folding the index would give one SSID a different series on each dialect, and a controller
/// replaced by another vendor's would start its history again.
///
/// 🚨 **Changing this function re-keys every stored series and orphans every row name.** It is
/// pinned to literal values by a test, the same way [`ap_id`] is pinned to its MAC.
#[must_use]
pub fn ssid_row_key(name: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.as_bytes() {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    // 0 is reserved: a row key of zero reads as "no row" at several edges, and one SSID in four
    // billion is not worth the ambiguity.
    if h == 0 {
        1
    } else {
        h
    }
}

/// The built-in profile an imported AP node carries (ADR-064). It attaches no template: an AP is
/// never polled itself — its controller's walk answers for it.
pub const WIRELESS_AP_PROFILE: &str = "Wireless AP (via controller)";

/// The AP cap a controller gets unless an operator sets another (ADR-064 決定 8 / 改訂 R12).
pub const MAX_APS_PER_CONTROLLER_DEFAULT: u32 = 1024;

/// The most APs one controller result may carry, whatever an operator asks for.
///
/// Sized from the payload, not from taste. Re-measured after ADR-064 増分 E added two readings:
/// one observation with the strings and numbers a real AC answers serializes to about 340 bytes of
/// JSON, so 2,048 of them are **705 KB** against NATS's 1 MiB `max_payload` — still inside
/// [`WLAN_INVENTORY_BYTE_BUDGET`], with room for the rest of the result.
///
/// ⚠️ **A field added to [`WlanApObservation`] spends this headroom**, and the count cap is not what
/// stops it: at the worst-case string length the byte budget already cuts the list well below 2,048.
/// `a_controller_result_at_the_hard_cap_fits_the_bus` in the bus crate measures both cases, and the
/// realistic one is realistic about **numbers** as well as strings — a ten-digit temperature is not
/// a measurement, and letting the fixture keep one hid how much of the budget a reading costs.
pub const MAX_APS_PER_CONTROLLER_HARD: u32 = 2048;

/// The longest device string kept on an observation (name, serial, model, version, group), in
/// characters. The same bound the row-name walk applies.
pub const WLAN_TEXT_MAX_CHARS: usize = crate::row_names::ROW_NAME_MAX_CHARS;

/// The most bytes of serialized observations one inventory may carry.
///
/// 🚨 **The AP count alone does not bound the message.** Measured: 2,048 observations of realistic
/// length are about 400 KB, but with every string at [`WLAN_TEXT_MAX_CHARS`] they are 1.6 MB — past
/// NATS's 1 MiB `max_payload`, and a publish that is too large is lost rather than shortened. So the
/// list is cut at whichever comes first, the count or this budget, and says where
/// ([`WlanInventory::truncated_at`]). 800 KB leaves room for the rest of the result.
pub const WLAN_INVENTORY_BYTE_BUDGET: usize = 800_000;

/// An access point's MAC address, the identity every source can produce.
///
/// Serialized as lower-case, colon-separated hex (`54:f6:e2:83:50:80`), which is also how Meraki's
/// Dashboard API spells it, so a later Meraki adapter parses the same string into the same value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ApMac([u8; 6]);

impl ApMac {
    /// The six bytes, as given.
    #[must_use]
    pub const fn new(bytes: [u8; 6]) -> Self {
        Self(bytes)
    }

    /// The bytes.
    #[must_use]
    pub const fn bytes(self) -> [u8; 6] {
        self.0
    }

    /// From six SNMP sub-identifiers — how a MAC-indexed table spells its row (`…84.246.226.131.80.128`).
    /// `None` unless there are exactly six and each fits a byte.
    #[must_use]
    pub fn from_subids(subids: &[u32]) -> Option<Self> {
        let bytes: [u32; 6] = subids.try_into().ok()?;
        let mut out = [0u8; 6];
        for (o, b) in out.iter_mut().zip(bytes) {
            *o = u8::try_from(b).ok()?;
        }
        Some(Self(out))
    }

    /// Parse `aa:bb:cc:dd:ee:ff` or `aa-bb-cc-dd-ee-ff`, in either case. `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.trim().split([':', '-']).collect();
        if parts.len() != 6 {
            return None;
        }
        let mut out = [0u8; 6];
        for (o, p) in out.iter_mut().zip(parts) {
            if p.len() != 2 {
                return None;
            }
            *o = u8::from_str_radix(p, 16).ok()?;
        }
        Some(Self(out))
    }
}

impl fmt::Display for ApMac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

impl From<ApMac> for String {
    fn from(mac: ApMac) -> Self {
        mac.to_string()
    }
}

impl TryFrom<String> for ApMac {
    type Error = String;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s).ok_or_else(|| format!("not a MAC address: {s:?}"))
    }
}

/// The namespace [`ap_id`] derives from. Fixed forever: changing it re-keys every AP node.
pub const WLAN_AP_NS: Uuid = Uuid::from_u128(0x5c1b_7a3e_064a_4d1e_9b2f_a9c0_0f3e_6401);

/// An access point's stable id — the identity of its node and of its inventory row (ADR-064 決定 8b).
///
/// 🚨 **Derived from the MAC alone.** Mixing the controller into the seed would give an AP a new id
/// at every HA switchover and split its history in two. Hashed over the raw bytes, so no spelling
/// of the MAC can change the answer.
#[must_use]
pub fn ap_id(mac: ApMac) -> Uuid {
    Uuid::new_v5(&WLAN_AP_NS, &mac.0)
}

/// Which vendor dialect a controller speaks, selected by the collection item's OID
/// (the [`crate::OpticalFlavor`] shape, ADR-064 決定 3).
///
/// Only dialects measured on a real controller are here. Cisco (AireOS and IOS-XE are two MIBs)
/// and Aruba are later increments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WlanFlavor {
    /// Huawei AC (HUAWEI-WLAN-AP-MIB) — measured on the AC6508, V200R024C00SPC100.
    Huawei,
}

impl WlanFlavor {
    /// Every dialect.
    pub const ALL: [Self; 1] = [Self::Huawei];

    /// The stored and serialized token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Huawei => "huawei",
        }
    }

    /// The inverse of [`Self::as_str`].
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == s)
    }

    /// The entry OID of the dialect's AP table — the collection item's `oid`.
    ///
    /// Huawei: `hwWlanApEntry`, `INDEX { hwWlanApMac }`. The MAC-indexed table rather than
    /// `hwWlanIDIndexedApTable`, because its index *is* the identity and the radio table's index
    /// starts with the same six sub-identifiers (verified on the PoC), so one key runs end to end
    /// and nothing depends on AP ids being stable across an HA pair.
    #[must_use]
    pub const fn root_oid(self) -> &'static str {
        match self {
            Self::Huawei => "1.3.6.1.4.1.2011.6.139.13.3.3.1",
        }
    }

    /// The entry OID of the dialect's **SSID statistics** table (ADR-064 増分 D).
    ///
    /// Huawei: `hwWlanSsidStatisticTable`, `INDEX { hwWlanSsid }` — the index *is* the SSID name,
    /// as a length-prefixed octet string. ⚠️ The name has no readable column: `.1` is
    /// not-accessible and answered nothing on the measured AC6508, which is why the poller decodes
    /// it out of the index rather than walking for it.
    #[must_use]
    pub const fn ssid_root_oid(self) -> &'static str {
        match self {
            Self::Huawei => "1.3.6.1.4.1.2011.6.139.17.1.2.1",
        }
    }

    /// The built-in collection template that walks this dialect's SSID table.
    #[must_use]
    pub const fn ssid_template_name(self) -> &'static str {
        match self {
            Self::Huawei => "Huawei WLAN SSIDs (AC)",
        }
    }

    /// The dialect an item's OID selects, or `None` — a job for an OID no dialect claims is skipped.
    #[must_use]
    pub fn from_root(oid: &str) -> Option<Self> {
        let oid = oid.trim_start_matches('.');
        Self::ALL
            .into_iter()
            .find(|f| f.root_oid() == oid || f.ssid_root_oid() == oid)
    }

    /// Whether `oid` is this dialect's SSID table rather than its AP table — what decides which
    /// walk a collection item asks for.
    #[must_use]
    pub fn is_ssid_root(self, oid: &str) -> bool {
        oid.trim_start_matches('.') == self.ssid_root_oid()
    }

    /// The built-in collection template that walks this dialect's AP table.
    #[must_use]
    pub const fn template_name(self) -> &'static str {
        match self {
            Self::Huawei => "Huawei WLAN access points (AC)",
        }
    }

    /// The vendor an AP imported from this dialect's controller is filed under — the node's
    /// `vendor` column (ADR-064 increment B2).
    #[must_use]
    pub const fn vendor(self) -> &'static str {
        match self {
            Self::Huawei => "Huawei",
        }
    }
}

/// What one controller says about one AP, reduced to the three answers the system acts on
/// (ADR-064 改訂 R5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WlanApState {
    /// This controller is serving the AP and its values are real.
    Associated,
    /// This controller is an HA standby for the AP. **Its values are zeros, not readings.**
    Backup,
    /// The controller knows the AP and reports it as not working (down, not yet joined, failed its
    /// configuration, wrong software…).
    NotAssociated,
}

impl WlanApState {
    /// Every state.
    pub const ALL: [Self; 3] = [Self::Associated, Self::Backup, Self::NotAssociated];

    /// The stored and serialized token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Associated => "associated",
            Self::Backup => "backup",
            Self::NotAssociated => "not_associated",
        }
    }

    /// The inverse of [`Self::as_str`].
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// Huawei `hwWlanApRunState` (HUAWEI-WLAN-AP-MIB), as `(value, token, state)`.
///
/// The token is the MIB's own label, kept so an operator sees what the controller said. Measured on
/// the PoC: the active AC answers `normal` for its 30 working APs and `fault` for 8; its HA standby
/// answers `standby` for the same 30 and `fault` for the same 8.
///
/// ⚠️ `committing` is a normal AP applying configuration and counts as associated, as the MIB's own
/// `hwWlanApNormalRatio` ("normal and commit states") does.
pub const HUAWEI_AP_RUN_STATES: [(i64, &str, WlanApState); 15] = [
    (1, "idle", WlanApState::NotAssociated),
    (2, "autofind", WlanApState::NotAssociated),
    (3, "type_not_match", WlanApState::NotAssociated),
    (4, "fault", WlanApState::NotAssociated),
    (5, "config", WlanApState::NotAssociated),
    (6, "config_failed", WlanApState::NotAssociated),
    (7, "download", WlanApState::NotAssociated),
    (8, "normal", WlanApState::Associated),
    (9, "committing", WlanApState::Associated),
    (10, "commit_failed", WlanApState::NotAssociated),
    (11, "standby", WlanApState::Backup),
    (12, "version_mismatch", WlanApState::NotAssociated),
    (13, "name_conflicted", WlanApState::NotAssociated),
    (14, "invalid", WlanApState::NotAssociated),
    (15, "country_code_mismatch", WlanApState::NotAssociated),
];

/// A run state value a Huawei AC answered, as `(token, state)`. A value outside the MIB is reported
/// as `unknown_<n>` and treated as not associated — never as serving.
#[must_use]
pub fn huawei_run_state(value: i64) -> (String, WlanApState) {
    HUAWEI_AP_RUN_STATES
        .iter()
        .find(|(v, _, _)| *v == value)
        .map_or_else(
            || (format!("unknown_{value}"), WlanApState::NotAssociated),
            |(_, token, state)| ((*token).to_owned(), *state),
        )
}

/// One AP as one controller saw it on one poll.
///
/// Descriptive fields are for PostgreSQL and the AP list — **never a TSDB label** (ADR-011). The
/// numbers are carried so core can publish them on the AP's own node once the AP is imported
/// (increment B2), and are `None` wherever the device answered its "no reading" value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WlanApObservation {
    /// The identity (ADR-064 決定 8b).
    pub mac: ApMac,
    /// The AP's name on the controller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Serial number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    /// Model, as the controller spells it (`AirEngine5776-26`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Software version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sw_version: Option<String>,
    /// Management address. `None` when the controller reports none (Huawei answers
    /// `255.255.255.255` for an AP that is down).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<IpAddr>,
    /// The vendor's own grouping of APs (a Huawei AP group).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_group: Option<String>,
    /// The controller's own word for the AP's state (`normal`, `fault`, `standby`).
    pub run_state: String,
    /// What that word means to Yagra.
    pub state: WlanApState,
    /// Wireless clients online through this AP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clients: Option<u32>,
    /// CPU in use, percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<u32>,
    /// Memory in use, percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_pct: Option<u32>,
    /// Operating temperature, °C. `None` on the many APs with no such sensor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_c: Option<i32>,
    /// CPU die temperature, °C — read from a **second, optional** walk (ADR-064 増分 E), so `None`
    /// also covers "this controller never answered that column".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_temp_c: Option<i32>,
    /// Power supply state, the MIB's own enumeration (see [`METRIC_WLAN_AP_POWER_STATE`]). From the
    /// optional walk, so `None` also means the column went unanswered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_state: Option<u32>,
}

impl WlanApObservation {
    /// Re-apply the string bounds, for a reader that did not produce the observation itself
    /// (core, on receipt). A poller of another version is not trusted to have done it.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        let clean = |s: Option<String>| s.as_deref().and_then(sanitize_wlan_text);
        self.name = clean(self.name);
        self.serial = clean(self.serial);
        self.model = clean(self.model);
        self.sw_version = clean(self.sw_version);
        self.vendor_group = clean(self.vendor_group);
        self.run_state = sanitize_wlan_text(&self.run_state).unwrap_or_else(|| "unknown".into());
        self
    }
}

/// Everything one controller reported about its APs on one poll.
///
/// Published only when the walk got **every** column (ADR-064 決定 9b): a partial table would read
/// as APs disappearing. `None` on the result and `Some(empty)` mean different things, as for the
/// neighbour set — `None` is "no inventory this poll", `Some(empty)` is "this controller manages no
/// APs".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WlanInventory {
    /// The dialect that produced it.
    #[serde(default = "default_flavor")]
    pub flavor: WlanFlavor,
    /// The APs, sorted by MAC so two pollers reading one controller publish the same list.
    #[serde(default)]
    pub aps: Vec<WlanApObservation>,
    /// Set when the controller reported more APs than the cap: how many it reported. The list then
    /// holds the first `cap` by MAC. A controller over its cap is shown as such, never silently cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_at: Option<u32>,
}

impl WlanInventory {
    /// The inventory to publish from every AP a controller reported: sorted by MAC, cut at `max_aps`
    /// **and** at [`WLAN_INVENTORY_BYTE_BUDGET`], whichever comes first.
    ///
    /// Sorting first is what makes a cut deterministic — two pollers reading one controller keep the
    /// same APs — and `truncated_at` carries how many the controller reported, so a controller over
    /// either bound is shown as such rather than silently managing fewer APs than it does.
    /// `max_aps` is itself capped at [`MAX_APS_PER_CONTROLLER_HARD`].
    #[must_use]
    pub fn bounded(flavor: WlanFlavor, mut aps: Vec<WlanApObservation>, max_aps: u32) -> Self {
        aps.sort_by_key(|a| a.mac);
        aps.dedup_by_key(|a| a.mac);
        let reported = aps.len();
        let cap = usize::try_from(max_aps.min(MAX_APS_PER_CONTROLLER_HARD)).unwrap_or(usize::MAX);
        let mut bytes = 0usize;
        let mut kept = 0usize;
        for ap in aps.iter().take(cap) {
            // Serializing a plain struct of strings and numbers cannot fail; an error would only
            // overstate the size, which cuts sooner — the safe direction.
            bytes += serde_json::to_vec(ap).map_or(WLAN_INVENTORY_BYTE_BUDGET, |v| v.len() + 1);
            if bytes > WLAN_INVENTORY_BYTE_BUDGET {
                break;
            }
            kept += 1;
        }
        aps.truncate(kept);
        Self {
            flavor,
            aps,
            truncated_at: (kept < reported).then(|| u32::try_from(reported).unwrap_or(u32::MAX)),
        }
    }
}

fn default_flavor() -> WlanFlavor {
    WlanFlavor::Huawei
}

impl Default for WlanFlavor {
    fn default() -> Self {
        default_flavor()
    }
}

/// Clean a device string for storage and display. See [`sanitize_row_name`].
#[must_use]
pub fn sanitize_wlan_text(raw: &str) -> Option<String> {
    sanitize_row_name(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mac_reads_the_same_from_every_spelling() {
        let from_index = ApMac::from_subids(&[84, 246, 226, 131, 80, 128]).expect("six bytes");
        assert_eq!(from_index.to_string(), "54:f6:e2:83:50:80");
        assert_eq!(ApMac::parse("54:F6:E2:83:50:80"), Some(from_index));
        assert_eq!(ApMac::parse("54-f6-e2-83-50-80"), Some(from_index));
        assert_eq!(
            ap_id(from_index),
            ap_id(ApMac::parse("54:f6:e2:83:50:80").unwrap())
        );
    }

    #[test]
    fn a_mac_that_is_not_six_bytes_is_refused() {
        assert_eq!(ApMac::from_subids(&[1, 2, 3, 4, 5]), None);
        assert_eq!(ApMac::from_subids(&[1, 2, 3, 4, 5, 6, 7]), None);
        assert_eq!(ApMac::from_subids(&[1, 2, 3, 4, 5, 256]), None);
        assert_eq!(ApMac::parse("54:f6:e2:83:50"), None);
        assert_eq!(ApMac::parse("54:f6:e2:83:50:8"), None);
        assert_eq!(ApMac::parse("zz:f6:e2:83:50:80"), None);
    }

    /// The id is a function of the MAC and of nothing else, and it never changes: a changed
    /// namespace or hash input would re-key every AP node in every deployment.
    #[test]
    fn an_ap_id_is_pinned_to_its_mac() {
        let mac = ApMac::new([0x54, 0xf6, 0xe2, 0x83, 0x50, 0x80]);
        assert_eq!(
            ap_id(mac).to_string(),
            Uuid::new_v5(&WLAN_AP_NS, &[0x54, 0xf6, 0xe2, 0x83, 0x50, 0x80]).to_string()
        );
        assert_ne!(
            ap_id(mac),
            ap_id(ApMac::new([0x54, 0xf6, 0xe2, 0x83, 0x50, 0xa0]))
        );
        assert_eq!(
            WLAN_AP_NS.to_string(),
            "5c1b7a3e-064a-4d1e-9b2f-a9c00f3e6401"
        );
    }

    #[test]
    fn a_mac_serializes_as_a_string_and_back() {
        let mac = ApMac::new([0x60, 0x10, 0x9e, 0x1e, 0xfc, 0xa0]);
        let json = serde_json::to_string(&mac).unwrap();
        assert_eq!(json, "\"60:10:9e:1e:fc:a0\"");
        assert_eq!(serde_json::from_str::<ApMac>(&json).unwrap(), mac);
        assert!(serde_json::from_str::<ApMac>("\"not a mac\"").is_err());
    }

    #[test]
    fn every_token_round_trips_through_its_parser_and_through_serde() {
        for f in WlanFlavor::ALL {
            assert_eq!(WlanFlavor::from_token(f.as_str()), Some(f));
            assert_eq!(
                serde_json::to_string(&f).unwrap(),
                format!("\"{}\"", f.as_str())
            );
            assert_eq!(WlanFlavor::from_root(f.root_oid()), Some(f));
            assert_eq!(
                WlanFlavor::from_root(&format!(".{}", f.root_oid())),
                Some(f)
            );
        }
        for s in WlanApState::ALL {
            assert_eq!(WlanApState::from_token(s.as_str()), Some(s));
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.as_str())
            );
        }
        assert_eq!(
            WlanFlavor::from_root("1.3.6.1.4.1.2011.6.139.13.3.10.1"),
            None
        );
    }

    /// The measured states land where the arbitration needs them: the active answers `normal`, the
    /// standby `standby`, and a broken AP `fault` on both.
    #[test]
    fn the_huawei_run_states_classify_as_measured_on_the_poc_pair() {
        assert_eq!(
            huawei_run_state(8),
            ("normal".into(), WlanApState::Associated)
        );
        assert_eq!(
            huawei_run_state(11),
            ("standby".into(), WlanApState::Backup)
        );
        assert_eq!(
            huawei_run_state(4),
            ("fault".into(), WlanApState::NotAssociated)
        );
        assert_eq!(
            huawei_run_state(99),
            ("unknown_99".into(), WlanApState::NotAssociated)
        );
        // The table is the MIB's enumeration, 1..=15, each value once.
        let values: Vec<i64> = HUAWEI_AP_RUN_STATES.iter().map(|(v, _, _)| *v).collect();
        assert_eq!(values, (1..=15).collect::<Vec<_>>());
        // Exactly one value means standby: anything else read as Backup would drop real values.
        assert_eq!(
            HUAWEI_AP_RUN_STATES
                .iter()
                .filter(|(_, _, s)| *s == WlanApState::Backup)
                .count(),
            1
        );
    }

    fn observation(mac: [u8; 6], text: &str) -> WlanApObservation {
        WlanApObservation {
            mac: ApMac::new(mac),
            name: Some(text.to_owned()),
            serial: Some(text.to_owned()),
            model: Some(text.to_owned()),
            sw_version: Some(text.to_owned()),
            ip: None,
            vendor_group: Some(text.to_owned()),
            run_state: "normal".into(),
            state: WlanApState::Associated,
            clients: Some(1),
            cpu_pct: None,
            mem_pct: None,
            temp_c: None,
            cpu_temp_c: None,
            power_state: None,
        }
    }

    #[test]
    fn an_inventory_under_its_bounds_keeps_every_ap_in_mac_order() {
        let inv = WlanInventory::bounded(
            WlanFlavor::Huawei,
            vec![
                observation([0, 0, 0, 0, 0, 2], "b"),
                observation([0, 0, 0, 0, 0, 1], "a"),
            ],
            1024,
        );
        assert_eq!(inv.aps.len(), 2);
        assert_eq!(inv.aps[0].mac, ApMac::new([0, 0, 0, 0, 0, 1]));
        assert_eq!(inv.truncated_at, None);
    }

    #[test]
    fn an_inventory_over_its_ap_cap_is_cut_and_says_how_many_there_were() {
        let aps = (0u8..10)
            .map(|i| observation([0, 0, 0, 0, 0, i], "ap"))
            .collect();
        let inv = WlanInventory::bounded(WlanFlavor::Huawei, aps, 4);
        assert_eq!(inv.aps.len(), 4);
        assert_eq!(inv.truncated_at, Some(10));
        // The first four by MAC, so another poller cuts the same way.
        assert_eq!(inv.aps[3].mac, ApMac::new([0, 0, 0, 0, 0, 3]));
    }

    /// The count is not enough: the byte budget cuts a list of long strings well before the count
    /// cap, and the result then serializes within the budget.
    #[test]
    fn an_inventory_of_long_strings_is_cut_by_the_byte_budget() {
        let long = "x".repeat(WLAN_TEXT_MAX_CHARS);
        let aps = (0..2048u16)
            .map(|i| {
                let [hi, lo] = i.to_be_bytes();
                observation([0, 0, 0, 0, hi, lo], &long)
            })
            .collect();
        let inv = WlanInventory::bounded(WlanFlavor::Huawei, aps, MAX_APS_PER_CONTROLLER_HARD);
        assert!(inv.aps.len() < 2048, "{}", inv.aps.len());
        assert!(!inv.aps.is_empty());
        assert_eq!(inv.truncated_at, Some(2048));
        assert!(serde_json::to_vec(&inv).unwrap().len() <= WLAN_INVENTORY_BYTE_BUDGET + 64);
    }

    #[test]
    fn a_cap_above_the_hard_cap_is_held_to_it() {
        let aps = (0..2100u16)
            .map(|i| {
                let [hi, lo] = i.to_be_bytes();
                observation([0, 0, 0, 0, hi, lo], "a")
            })
            .collect();
        let inv = WlanInventory::bounded(WlanFlavor::Huawei, aps, u32::MAX);
        assert_eq!(inv.aps.len(), 2048);
        assert_eq!(inv.truncated_at, Some(2100));
    }

    #[test]
    fn a_received_observation_is_cleaned_again() {
        let raw = WlanApObservation {
            mac: ApMac::new([1, 2, 3, 4, 5, 6]),
            name: Some("ap\u{0}001\n".into()),
            serial: Some("   ".into()),
            model: Some("x".repeat(500)),
            sw_version: None,
            ip: None,
            vendor_group: None,
            run_state: "\u{7}".into(),
            state: WlanApState::Associated,
            clients: Some(3),
            cpu_pct: None,
            mem_pct: None,
            temp_c: None,
            cpu_temp_c: None,
            power_state: None,
        };
        let clean = raw.sanitized();
        assert_eq!(clean.name.as_deref(), Some("ap 001"));
        assert_eq!(clean.serial, None);
        assert_eq!(
            clean.model.map(|m| m.chars().count()),
            Some(WLAN_TEXT_MAX_CHARS)
        );
        assert_eq!(clean.run_state, "unknown");
    }
}
