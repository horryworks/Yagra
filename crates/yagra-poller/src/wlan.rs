// SPDX-License-Identifier: AGPL-3.0-only
//! A wireless controller's AP table, read into an inventory (ADR-064).
//!
//! Pure functions over already-walked rows, the split `optical.rs` and `mau.rs` use — the SNMP
//! session lives in [`crate::worker`] and everything worth testing lives here. What can go wrong
//! quietly:
//!
//! - **the identity**, because the row index *is* the AP's MAC (`hwWlanApEntry` is
//!   `INDEX { hwWlanApMac }`) and a row whose index is not six bytes must be dropped rather than
//!   hashed into a key that names nobody;
//! - **a torn table**, because the columns arrive separately. An AP exists in the inventory only if
//!   the controller gave it a **run state** — a name or a client count with no state would publish
//!   an AP whose serving controller nobody could decide;
//! - **the vendor's "no reading" values**, which are numbers that look like readings: Huawei answers
//!   `255` for the temperature of an AP with no sensor (36 of 38 on the PoC) and `255.255.255.255`
//!   for the address of an AP that is down (ADR-064 改訂 R10).
//!
//! ⚠️ **What a standby controller answers is not filtered here.** Its APs read `standby` with CPU,
//! memory and radio values of 0 — measured — and the inventory reports that faithfully as
//! [`WlanApState::Backup`]. Deciding that a backup's numbers are not readings needs every controller's
//! view of the AP at once, which only core has.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use yagra_bus::{RowName, Sample};
use yagra_common::{
    huawei_run_state, sanitize_wlan_text, ssid_row_key, ApMac, MetricKind, WlanApObservation,
    WlanFlavor, WlanInventory, MAX_APS_PER_CONTROLLER_HARD, METRIC_WLAN_SSID_AP_COUNT,
    METRIC_WLAN_SSID_CLIENTS, METRIC_WLAN_SSID_CLIENTS_2G4, METRIC_WLAN_SSID_CLIENTS_5G,
    METRIC_WLAN_SSID_CLIENTS_6G, METRIC_WLAN_SSID_IN_OCTETS, METRIC_WLAN_SSID_OUT_OCTETS,
};
use yagra_transport::{SnmpInstanceRow, SnmpValue};

/// Huawei `hwWlanApEntry` columns that **must all answer**, as `(column number, field)`. Numbers
/// measured on the PoC's AC6508 and matching HUAWEI-WLAN-AP-MIB.
///
/// This list, and only this list, decides `wlan_ap_walk_complete` and therefore whether an
/// inventory is published at all (ADR-064 決定 9b). Anything whose absence should cost one reading
/// rather than the whole AP list belongs in [`HUAWEI_OPTIONAL_COLUMNS`].
const HUAWEI_COLUMNS: [(u32, Field); 11] = [
    (6, Field::RunState),
    (4, Field::Name),
    (2, Field::Serial),
    (3, Field::Model),
    (5, Field::Group),
    (7, Field::SwVersion),
    (13, Field::Ip),
    (44, Field::Clients),
    (41, Field::Cpu),
    (40, Field::Mem),
    (43, Field::Temp),
];

/// Huawei `hwWlanApEntry` columns read in a **second walk**, whose absence costs only the readings
/// they carry (ADR-064 増分 E).
///
/// 🚨 **They are kept out of [`HUAWEI_COLUMNS`] deliberately, and this is not tidiness.**
/// [`yagra_transport::InstanceWalk::every_column_answered`] is one bool for the whole walk, and
/// 決定 9b throws the entire inventory away when it is false. Put an optional column in the
/// required walk and any Huawei model that does not implement it stops having an AP list at all —
/// not "loses a reading". That failure is measured, not imagined: on the PoC's AC6508 the columns
/// `.19`, `.42` and `.50`–`.53` were asked for and never answered, so a model that skips `.83` is
/// an ordinary expectation rather than a worry.
const HUAWEI_OPTIONAL_COLUMNS: [(u32, Field); 2] = [(83, Field::CpuTemp), (80, Field::PowerState)];

/// What a column contributes to an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    RunState,
    Name,
    Serial,
    Model,
    Group,
    SwVersion,
    Ip,
    Clients,
    Cpu,
    Mem,
    Temp,
    CpuTemp,
    PowerState,
}

/// Huawei's `hwWlanApTemperature` for an AP with no sensor. Measured: 36 of the PoC's 38 APs.
///
/// The same number is `hwWlanApCpuTemperature`'s placeholder, where it marks the 8 APs that were
/// down rather than 36 with no sensor — one value, two reasons, both meaning "not a reading".
const HUAWEI_NO_TEMPERATURE: i64 = 255;

/// The most rows one AP walk takes, across every column.
///
/// Twice the hard AP cap per column, so a controller slightly over its cap still yields a complete
/// walk — the inventory is then cut and says so. A controller with more APs than this reads as an
/// incomplete walk (`wlan_ap_walk_complete` = 0), never as a partial list.
#[must_use]
pub fn walk_row_budget(flavor: WlanFlavor) -> usize {
    per_column_row_budget() * columns(flavor).len()
}

/// The most rows the optional walk takes, sized the same way as [`walk_row_budget`].
#[must_use]
pub fn optional_walk_row_budget(flavor: WlanFlavor) -> usize {
    per_column_row_budget() * optional_columns(flavor).len()
}

fn per_column_row_budget() -> usize {
    usize::try_from(MAX_APS_PER_CONTROLLER_HARD).unwrap_or(usize::MAX) * 2
}

/// The column OIDs to walk for a dialect, run state first.
#[must_use]
pub fn columns(flavor: WlanFlavor) -> Vec<String> {
    oids(flavor, &HUAWEI_COLUMNS)
}

/// The column OIDs of the second, optional walk ([`HUAWEI_OPTIONAL_COLUMNS`]).
#[must_use]
pub fn optional_columns(flavor: WlanFlavor) -> Vec<String> {
    oids(flavor, &HUAWEI_OPTIONAL_COLUMNS)
}

fn oids(flavor: WlanFlavor, cols: &[(u32, Field)]) -> Vec<String> {
    match flavor {
        WlanFlavor::Huawei => cols
            .iter()
            .map(|(n, _)| format!("{}.{n}", flavor.root_oid()))
            .collect(),
    }
}

/// The inventory in a dialect's rows, bounded to `max_aps` and the byte budget
/// ([`WlanInventory::bounded`]).
///
/// `optional` carries the second walk's rows and may be empty — that walk is allowed to fail, and
/// an empty slice is exactly what "it did" looks like here. Both are folded in **before** bounding,
/// so the byte budget measures the observations that will actually be sent.
#[must_use]
pub fn inventory(
    flavor: WlanFlavor,
    rows: &[SnmpInstanceRow],
    optional: &[SnmpInstanceRow],
    max_aps: u32,
) -> WlanInventory {
    match flavor {
        WlanFlavor::Huawei => {
            WlanInventory::bounded(flavor, huawei_observations(flavor, rows, optional), max_aps)
        }
    }
}

fn huawei_observations(
    flavor: WlanFlavor,
    rows: &[SnmpInstanceRow],
    optional: &[SnmpInstanceRow],
) -> Vec<WlanApObservation> {
    let field_of: BTreeMap<String, Field> = HUAWEI_COLUMNS
        .iter()
        .chain(HUAWEI_OPTIONAL_COLUMNS.iter())
        .map(|(n, f)| (format!("{}.{n}", flavor.root_oid()), *f))
        .collect();
    // Keyed by MAC, the run state creating the entry. Other columns only fill an existing one, and
    // are collected first so their order in `rows` does not matter.
    let mut states: BTreeMap<ApMac, (String, yagra_common::WlanApState)> = BTreeMap::new();
    let mut rest: BTreeMap<ApMac, Vec<(Field, &SnmpValue)>> = BTreeMap::new();
    for row in rows.iter().chain(optional.iter()) {
        let Some(field) = field_of.get(row.oid_base.trim_start_matches('.')) else {
            continue;
        };
        let Some(mac) = ApMac::from_subids(&row.instance) else {
            continue;
        };
        if *field == Field::RunState {
            if let SnmpValue::Int(v) = row.value {
                states.insert(mac, huawei_run_state(v));
            }
        } else {
            rest.entry(mac).or_default().push((*field, &row.value));
        }
    }
    states
        .into_iter()
        .map(|(mac, (run_state, state))| {
            let mut obs = WlanApObservation {
                mac,
                name: None,
                serial: None,
                model: None,
                sw_version: None,
                ip: None,
                vendor_group: None,
                run_state,
                state,
                clients: None,
                cpu_pct: None,
                mem_pct: None,
                temp_c: None,
                cpu_temp_c: None,
                power_state: None,
            };
            for (field, value) in rest.remove(&mac).unwrap_or_default() {
                match field {
                    Field::RunState => {}
                    Field::Name => obs.name = text(value),
                    Field::Serial => obs.serial = text(value),
                    Field::Model => obs.model = text(value),
                    Field::Group => obs.vendor_group = text(value),
                    Field::SwVersion => obs.sw_version = text(value),
                    Field::Ip => obs.ip = address(value),
                    Field::Clients => obs.clients = non_negative(value),
                    Field::Cpu => obs.cpu_pct = non_negative(value),
                    Field::Mem => obs.mem_pct = non_negative(value),
                    Field::Temp => obs.temp_c = temperature(value),
                    Field::CpuTemp => obs.cpu_temp_c = temperature(value),
                    Field::PowerState => obs.power_state = non_negative(value),
                }
            }
            obs
        })
        .collect()
}

/// A device string, cleaned. Octets that are not UTF-8 are replaced rather than refused: a name is
/// still recognisable with one character lost.
fn text(value: &SnmpValue) -> Option<String> {
    match value {
        SnmpValue::Bytes(b) => sanitize_wlan_text(&String::from_utf8_lossy(b)),
        SnmpValue::Int(_) | SnmpValue::Oid(_) => None,
    }
}

/// An `IpAddress` column (four octets). `255.255.255.255` and `0.0.0.0` are the controller saying it
/// has no address for the AP.
fn address(value: &SnmpValue) -> Option<IpAddr> {
    let SnmpValue::Bytes(b) = value else {
        return None;
    };
    let octets: [u8; 4] = b.as_slice().try_into().ok()?;
    let ip = Ipv4Addr::from(octets);
    (!ip.is_broadcast() && !ip.is_unspecified()).then_some(IpAddr::V4(ip))
}

/// A temperature column, with the vendor's "no sensor"/"no reading" placeholder taken out.
///
/// One function for both temperature columns so they can never come to disagree about what `255`
/// means — it is the same placeholder in both (ADR-064 改訂 R10).
fn temperature(value: &SnmpValue) -> Option<i32> {
    match value {
        SnmpValue::Int(HUAWEI_NO_TEMPERATURE) => None,
        SnmpValue::Int(v) => i32::try_from(*v).ok(),
        SnmpValue::Bytes(_) | SnmpValue::Oid(_) => None,
    }
}

fn non_negative(value: &SnmpValue) -> Option<u32> {
    match value {
        SnmpValue::Int(v) => u32::try_from(*v).ok(),
        SnmpValue::Bytes(_) | SnmpValue::Oid(_) => None,
    }
}

// ─── SSID statistics (ADR-064 増分 D) ──────────────────────────────────────────

/// Huawei `hwWlanSsidStatisticEntry` columns read, as `(column number, field)`.
///
/// Left out, each for a reason: `.15`/`.16` (send/receive rate) carry **no unit in the MIB**, which
/// is the judgement `collection.rs` already made about `hwWlanGlobalUpSpeed` — and the byte
/// counters below give the same answer through `rate()` (ADR-012); `.6`/`.8`/`.11`–`.14` are
/// statistics over the controller's own echo interval, whose length is a device setting, so two
/// controllers' numbers are not comparable; `.7`/`.10` are frame counts, which the bytes cover.
const HUAWEI_SSID_COLUMNS: [(u32, SsidField); 6] = [
    (4, SsidField::ApCount),
    (2, SsidField::Clients2g4),
    (3, SsidField::Clients5g),
    (17, SsidField::Clients6g),
    (5, SsidField::InOctets),
    (9, SsidField::OutOctets),
];

/// What an SSID column contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SsidField {
    ApCount,
    Clients2g4,
    Clients5g,
    Clients6g,
    InOctets,
    OutOctets,
}

/// The most SSIDs one controller may publish.
///
/// 🚨 **Sized from the row-name budget, not from taste.** Every SSID needs one [`RowName`] per
/// metric for its name to join, so this walk asks for `WLAN_SSID_ROW_METRICS.len()` names per SSID
/// against `ROW_NAMES_MAX` = 512: 7 × 64 = 448, with room to spare. Raise this and the names of the
/// SSIDs past the cap are silently dropped on receipt — the values would arrive as bare numbers
/// with nothing to call them. The measured AC broadcasts 4.
pub const WLAN_SSID_MAX: usize = 64;

/// One SSID as one controller reported it, with the row key its series carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WlanSsidReading {
    /// The SSID, decoded from the table index and cleaned.
    pub name: String,
    /// [`yagra_common::ssid_row_key`] of `name`.
    pub row: u32,
    pub ap_count: Option<u32>,
    pub clients_2g4: Option<u32>,
    pub clients_5g: Option<u32>,
    pub clients_6g: Option<u32>,
    pub in_octets: Option<u64>,
    pub out_octets: Option<u64>,
}

impl WlanSsidReading {
    /// Clients over every band the controller answered for, or `None` when it answered for none.
    ///
    /// Summing only what was answered is deliberate: a controller with no 6 GHz radios omits that
    /// column, and treating the absence as zero would be right by luck rather than by evidence.
    #[must_use]
    pub fn clients(&self) -> Option<u32> {
        let parts = [self.clients_2g4, self.clients_5g, self.clients_6g];
        parts
            .iter()
            .any(Option::is_some)
            .then(|| parts.iter().flatten().sum())
    }
}

/// The SSID column OIDs to walk for a dialect.
#[must_use]
pub fn ssid_columns(flavor: WlanFlavor) -> Vec<String> {
    match flavor {
        WlanFlavor::Huawei => HUAWEI_SSID_COLUMNS
            .iter()
            .map(|(n, _)| format!("{}.{n}", flavor.ssid_root_oid()))
            .collect(),
    }
}

/// The most rows the SSID walk takes, across every column.
#[must_use]
pub fn ssid_walk_row_budget(flavor: WlanFlavor) -> usize {
    ssid_columns(flavor).len() * WLAN_SSID_MAX * 2
}

/// The SSIDs in a dialect's rows, in row-key order and bounded to [`WLAN_SSID_MAX`].
///
/// 🚨 **A row key collision drops the row rather than merging it.** Two SSIDs whose names hash the
/// same would otherwise share one series, and the result would look like one SSID with somebody
/// else's client count — a wrong number, where a missing one is only a gap.
#[must_use]
pub fn ssids(flavor: WlanFlavor, rows: &[SnmpInstanceRow]) -> Vec<WlanSsidReading> {
    match flavor {
        WlanFlavor::Huawei => huawei_ssids(flavor, rows),
    }
}

fn huawei_ssids(flavor: WlanFlavor, rows: &[SnmpInstanceRow]) -> Vec<WlanSsidReading> {
    let field_of: BTreeMap<String, SsidField> = HUAWEI_SSID_COLUMNS
        .iter()
        .map(|(n, f)| (format!("{}.{n}", flavor.ssid_root_oid()), *f))
        .collect();
    let mut by_name: BTreeMap<String, Vec<(SsidField, &SnmpValue)>> = BTreeMap::new();
    for row in rows {
        let Some(field) = field_of.get(row.oid_base.trim_start_matches('.')) else {
            continue;
        };
        let Some(name) = ssid_from_index(&row.instance) else {
            continue;
        };
        by_name.entry(name).or_default().push((*field, &row.value));
    }
    let mut out: Vec<WlanSsidReading> = Vec::new();
    let mut seen: BTreeMap<u32, String> = BTreeMap::new();
    for (name, fields) in by_name {
        let row = ssid_row_key(&name);
        if let Some(other) = seen.get(&row) {
            tracing::warn!(
                ssid = %name,
                clashes_with = %other,
                "two SSIDs share a row key; the second is dropped rather than merged"
            );
            metrics::counter!("yagra_wlan_ssid_row_key_collisions_total").increment(1);
            continue;
        }
        seen.insert(row, name.clone());
        let mut r = WlanSsidReading {
            name,
            row,
            ap_count: None,
            clients_2g4: None,
            clients_5g: None,
            clients_6g: None,
            in_octets: None,
            out_octets: None,
        };
        for (field, value) in fields {
            match field {
                SsidField::ApCount => r.ap_count = non_negative(value),
                SsidField::Clients2g4 => r.clients_2g4 = non_negative(value),
                SsidField::Clients5g => r.clients_5g = non_negative(value),
                SsidField::Clients6g => r.clients_6g = non_negative(value),
                SsidField::InOctets => r.in_octets = counter64(value),
                SsidField::OutOctets => r.out_octets = counter64(value),
            }
        }
        out.push(r);
    }
    out.sort_by_key(|r| r.row);
    out.truncate(WLAN_SSID_MAX);
    out
}

/// The SSID name inside a table index: a length-prefixed octet string, cleaned.
///
/// ⚠️ The length byte is checked against what follows rather than trusted — a controller that
/// disagrees with itself about the length would otherwise name an SSID out of neighbouring
/// sub-identifiers.
fn ssid_from_index(instance: &[u32]) -> Option<String> {
    let (len, rest) = instance.split_first()?;
    if *len as usize != rest.len() {
        return None;
    }
    let bytes: Vec<u8> = rest
        .iter()
        .map(|b| u8::try_from(*b).unwrap_or(b'?'))
        .collect();
    sanitize_wlan_text(&String::from_utf8_lossy(&bytes))
}

fn counter64(value: &SnmpValue) -> Option<u64> {
    match value {
        SnmpValue::Int(v) => u64::try_from(*v).ok(),
        SnmpValue::Bytes(_) | SnmpValue::Oid(_) => None,
    }
}

/// The samples and row names one controller's SSIDs stand for.
///
/// ⚠️ **Every SSID gets its names, including one with no clients at all.** The row-name *walk*
/// skips rows whose value was zero (a switch reports hundreds of entity rows with no memory), and
/// copying that rule here would delete exactly the SSID an operator is looking for — an empty
/// guest network is a fact, not noise. This path names what it publishes, unconditionally.
#[must_use]
pub fn ssid_samples(readings: &[WlanSsidReading]) -> (Vec<Sample>, Vec<RowName>) {
    let mut samples = Vec::new();
    let mut names = Vec::new();
    for r in readings {
        let gauges = [
            (METRIC_WLAN_SSID_CLIENTS, r.clients().map(f64::from)),
            (METRIC_WLAN_SSID_CLIENTS_2G4, r.clients_2g4.map(f64::from)),
            (METRIC_WLAN_SSID_CLIENTS_5G, r.clients_5g.map(f64::from)),
            (METRIC_WLAN_SSID_CLIENTS_6G, r.clients_6g.map(f64::from)),
            (METRIC_WLAN_SSID_AP_COUNT, r.ap_count.map(f64::from)),
        ];
        let counters = [
            (METRIC_WLAN_SSID_IN_OCTETS, r.in_octets),
            (METRIC_WLAN_SSID_OUT_OCTETS, r.out_octets),
        ];
        for (metric, value) in gauges {
            if let Some(v) = value {
                samples.push(Sample::interface(
                    metric,
                    yagra_common::IfIndex(r.row),
                    v,
                    MetricKind::Gauge,
                ));
                names.push(RowName {
                    metric: metric.to_owned(),
                    row: r.row,
                    name: r.name.clone(),
                });
            }
        }
        for (metric, value) in counters {
            if let Some(v) = value {
                #[allow(clippy::cast_precision_loss)]
                samples.push(Sample::interface(
                    metric,
                    yagra_common::IfIndex(r.row),
                    v as f64,
                    MetricKind::Counter,
                ));
                names.push(RowName {
                    metric: metric.to_owned(),
                    row: r.row,
                    name: r.name.clone(),
                });
            }
        }
    }
    (samples, names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::WlanApState;

    const ROOT: &str = "1.3.6.1.4.1.2011.6.139.13.3.3.1";
    const SSID_ROOT: &str = "1.3.6.1.4.1.2011.6.139.17.1.2.1";

    fn row(column: u32, mac: [u32; 6], value: SnmpValue) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: format!("{ROOT}.{column}"),
            instance: mac.to_vec(),
            value,
        }
    }

    fn bytes(s: &str) -> SnmpValue {
        SnmpValue::Bytes(s.as_bytes().to_vec())
    }

    /// Three APs shaped like the PoC's active controller: a working one, one down, and one with a
    /// temperature sensor. Names, serials and addresses are made up.
    /// The second walk fills the readings it carries, and the vendor placeholder is taken out of
    /// the CPU temperature exactly as it is out of the operating one (ADR-064 増分 E).
    ///
    /// Measured shape: on the PoC the 30 serving APs answered `.83` with 55-69 degrees and the 8
    /// that were down answered 255, which is why the placeholder has to be dropped on this column
    /// too rather than only on `.43`.
    #[test]
    fn the_optional_walk_fills_its_readings_and_drops_the_placeholder() {
        let up = [84, 246, 226, 131, 80, 128];
        let warm = [96, 16, 158, 31, 186, 96];
        let optional = vec![
            row(83, up, SnmpValue::Int(66)),
            row(80, up, SnmpValue::Int(1)),
            row(83, warm, SnmpValue::Int(255)),
        ];
        let inv = inventory(
            WlanFlavor::Huawei,
            &active_controller_rows(),
            &optional,
            1024,
        );
        let ap = |name: &str| {
            inv.aps
                .iter()
                .find(|a| a.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("{name} is in the inventory"))
                .clone()
        };
        assert_eq!(ap("site-ap-001").cpu_temp_c, Some(66));
        assert_eq!(ap("site-ap-001").power_state, Some(1));
        assert_eq!(
            ap("site-ap-014").cpu_temp_c,
            None,
            "255 is the controller saying it has no reading, not a temperature"
        );
        assert_eq!(
            ap("site-ap-014").power_state,
            None,
            "a column the second walk did not answer for this AP stays absent"
        );
    }
    /// The four SSIDs of the measured AC6508, as the recording holds them.
    ///
    /// Index = the SSID name as a length-prefixed octet string, which is the only place the name
    /// exists: column 1 is not-accessible and answered nothing on the real controller.
    fn ssid_rows() -> Vec<SnmpInstanceRow> {
        let idx = |name: &str| {
            let mut v = vec![u32::try_from(name.len()).unwrap()];
            v.extend(name.bytes().map(u32::from));
            v
        };
        let cell = |col: u32, name: &str, v: i64| SnmpInstanceRow {
            oid_base: format!("{SSID_ROOT}.{col}"),
            instance: idx(name),
            value: SnmpValue::Int(v),
        };
        vec![
            // lixilguest: 2 on 2.4 GHz, 11 on 5 GHz, broadcast by 30 APs.
            cell(2, "lixilguest", 2),
            cell(3, "lixilguest", 11),
            cell(17, "lixilguest", 0),
            cell(4, "lixilguest", 30),
            cell(5, "lixilguest", 283_912_285_339),
            cell(9, "lixilguest", 214_712_781_505_206),
            // global5: 2 clients, all on 5 GHz.
            cell(2, "global5", 0),
            cell(3, "global5", 2),
            cell(4, "global5", 30),
            // global24 and lixil-biz: broadcast by every AP, nobody on them.
            cell(2, "global24", 0),
            cell(3, "global24", 0),
            cell(4, "global24", 30),
            cell(2, "lixil-biz", 0),
            cell(3, "lixil-biz", 0),
            cell(4, "lixil-biz", 30),
        ]
    }

    #[test]
    fn the_ssid_table_reads_into_one_reading_per_ssid_with_its_name() {
        let ssids = ssids(WlanFlavor::Huawei, &ssid_rows());
        let names: Vec<&str> = ssids.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names.len(), 4, "{names:?}");
        let find = |n: &str| {
            ssids
                .iter()
                .find(|r| r.name == n)
                .unwrap_or_else(|| panic!("{n} decoded"))
        };
        assert_eq!(find("lixilguest").clients_2g4, Some(2));
        assert_eq!(find("lixilguest").clients_5g, Some(11));
        assert_eq!(find("lixilguest").clients(), Some(13));
        assert_eq!(find("lixilguest").in_octets, Some(283_912_285_339));
        assert_eq!(find("global5").clients(), Some(2));
        // The measured total: the four SSIDs add up to what the controller reports fleet-wide
        // in `wlan_controller_clients`, which is the check that the decode is not off by a row.
        let total: u32 = ssids.iter().filter_map(WlanSsidReading::clients).sum();
        assert_eq!(total, 15);
    }

    /// 🚨 An SSID nobody is using is still an SSID, and is the one an operator is most likely to
    /// be looking for. The row-name **walk** drops rows whose value was zero; copying that rule
    /// onto this path would delete an empty guest network from the list.
    #[test]
    fn an_ssid_with_no_clients_keeps_its_row_and_its_name() {
        let ssids = ssids(WlanFlavor::Huawei, &ssid_rows());
        let (samples, names) = ssid_samples(&ssids);
        let empty = ssids
            .iter()
            .find(|r| r.name == "global24")
            .expect("the empty SSID is in the readings");
        assert_eq!(empty.clients(), Some(0));
        assert!(
            names
                .iter()
                .any(|n| n.row == empty.row && n.name == "global24"),
            "an SSID with no clients is still named"
        );
        assert!(
            samples
                .iter()
                .any(|s| s.ifindex == Some(yagra_common::IfIndex(empty.row)) && s.value == 0.0),
            "and still publishes its zero"
        );
        // Every sample carries a row key and every row key has a name, in both directions.
        for s in &samples {
            let row = s.ifindex.expect("an SSID sample is keyed by its row").0;
            assert!(
                names.iter().any(|n| n.row == row && n.metric == s.metric),
                "{} row {row} has no name",
                s.metric
            );
        }
    }

    /// A malformed index names nothing rather than naming neighbouring sub-identifiers.
    #[test]
    fn an_index_whose_length_disagrees_with_itself_is_dropped() {
        let row = SnmpInstanceRow {
            oid_base: format!("{SSID_ROOT}.4"),
            // Says nine bytes follow; three do.
            instance: vec![9, 97, 98, 99],
            value: SnmpValue::Int(1),
        };
        assert!(ssids(WlanFlavor::Huawei, &[row]).is_empty());
    }

    /// The row key is a fact stored in the TSDB and joined to a name in PostgreSQL: changing it
    /// orphans both. Pinned to literals for the same reason [`ap_id`] is.
    #[test]
    fn an_ssid_row_key_is_pinned_to_its_name() {
        assert_eq!(yagra_common::ssid_row_key("lixilguest"), 2_035_702_017);
        assert_eq!(yagra_common::ssid_row_key("global5"), 3_547_817_473);
        assert_ne!(
            yagra_common::ssid_row_key("global5"),
            yagra_common::ssid_row_key("global24")
        );
    }
    fn active_controller_rows() -> Vec<SnmpInstanceRow> {
        let up = [84, 246, 226, 131, 80, 128];
        let down = [96, 16, 158, 30, 252, 160];
        let warm = [96, 16, 158, 31, 186, 96];
        vec![
            row(6, up, SnmpValue::Int(8)),
            row(4, up, bytes("site-ap-001")),
            row(2, up, bytes("21500000000000000001")),
            row(3, up, bytes("AirEngine5776-26")),
            row(5, up, bytes("default")),
            row(7, up, bytes("V600R024C00SPC100")),
            row(13, up, SnmpValue::Bytes(vec![10, 0, 0, 27])),
            row(44, up, SnmpValue::Int(5)),
            row(41, up, SnmpValue::Int(3)),
            row(40, up, SnmpValue::Int(41)),
            row(43, up, SnmpValue::Int(255)),
            row(6, down, SnmpValue::Int(4)),
            row(4, down, bytes("site-ap-005")),
            row(13, down, SnmpValue::Bytes(vec![255, 255, 255, 255])),
            row(44, down, SnmpValue::Int(0)),
            row(43, down, SnmpValue::Int(255)),
            row(6, warm, SnmpValue::Int(8)),
            row(4, warm, bytes("site-ap-014")),
            row(43, warm, SnmpValue::Int(48)),
        ]
    }

    #[test]
    fn the_columns_are_the_measured_ones_under_the_ap_table() {
        let cols = columns(WlanFlavor::Huawei);
        assert_eq!(cols[0], format!("{ROOT}.6"), "run state first");
        assert_eq!(cols.len(), 11);
        assert!(cols.contains(&format!("{ROOT}.44")), "online users");
        assert!(walk_row_budget(WlanFlavor::Huawei) >= 11 * 2048);
    }

    #[test]
    fn an_active_controller_reads_into_one_observation_per_ap() {
        let inv = inventory(WlanFlavor::Huawei, &active_controller_rows(), &[], 1024);
        assert_eq!(inv.aps.len(), 3);
        assert_eq!(inv.truncated_at, None);
        let up = &inv.aps[0];
        assert_eq!(up.mac.to_string(), "54:f6:e2:83:50:80");
        assert_eq!(up.name.as_deref(), Some("site-ap-001"));
        assert_eq!(up.model.as_deref(), Some("AirEngine5776-26"));
        assert_eq!(up.sw_version.as_deref(), Some("V600R024C00SPC100"));
        assert_eq!(up.vendor_group.as_deref(), Some("default"));
        assert_eq!(up.ip, Some("10.0.0.27".parse().unwrap()));
        assert_eq!(
            (up.run_state.as_str(), up.state),
            ("normal", WlanApState::Associated)
        );
        assert_eq!(
            (up.clients, up.cpu_pct, up.mem_pct),
            (Some(5), Some(3), Some(41))
        );
        // 255 is "no sensor", not 255 °C.
        assert_eq!(up.temp_c, None);
    }

    #[test]
    fn an_ap_that_is_down_has_no_address_and_is_not_associated() {
        let inv = inventory(WlanFlavor::Huawei, &active_controller_rows(), &[], 1024);
        let down = inv
            .aps
            .iter()
            .find(|a| a.name.as_deref() == Some("site-ap-005"))
            .unwrap();
        assert_eq!(
            (down.run_state.as_str(), down.state),
            ("fault", WlanApState::NotAssociated)
        );
        assert_eq!(
            down.ip, None,
            "255.255.255.255 is the controller saying there is none"
        );
        // Zero clients is a reading, unlike the temperature placeholder.
        assert_eq!(down.clients, Some(0));
        let warm = inv
            .aps
            .iter()
            .find(|a| a.name.as_deref() == Some("site-ap-014"))
            .unwrap();
        assert_eq!(warm.temp_c, Some(48));
    }

    /// The standby's view of the same AP: reported as Backup with its zeros intact. Filtering them is
    /// core's job, because only core sees the active controller's answer beside it.
    #[test]
    fn a_standby_controller_reports_backup_with_its_values_as_given() {
        let mac = [84, 246, 226, 131, 80, 128];
        let rows = vec![
            row(6, mac, SnmpValue::Int(11)),
            row(44, mac, SnmpValue::Int(5)),
            row(41, mac, SnmpValue::Int(0)),
        ];
        let inv = inventory(WlanFlavor::Huawei, &rows, &[], 1024);
        assert_eq!(
            (inv.aps[0].run_state.as_str(), inv.aps[0].state),
            ("standby", WlanApState::Backup)
        );
        assert_eq!(inv.aps[0].cpu_pct, Some(0));
    }

    /// A torn table: values for an AP whose run state never arrived do not become an AP.
    #[test]
    fn a_row_with_no_run_state_is_not_an_ap() {
        let rows = vec![
            row(4, [1, 2, 3, 4, 5, 6], bytes("orphan")),
            row(44, [1, 2, 3, 4, 5, 6], SnmpValue::Int(9)),
        ];
        assert!(inventory(WlanFlavor::Huawei, &rows, &[], 1024)
            .aps
            .is_empty());
    }

    #[test]
    fn a_row_whose_index_is_not_a_mac_is_dropped() {
        let rows = vec![
            SnmpInstanceRow {
                oid_base: format!("{ROOT}.6"),
                instance: vec![1, 2, 3],
                value: SnmpValue::Int(8),
            },
            SnmpInstanceRow {
                oid_base: format!("{ROOT}.6"),
                instance: vec![1, 2, 3, 4, 5, 999],
                value: SnmpValue::Int(8),
            },
        ];
        assert!(inventory(WlanFlavor::Huawei, &rows, &[], 1024)
            .aps
            .is_empty());
    }

    #[test]
    fn device_strings_are_cleaned_and_non_utf8_survives() {
        let mac = [1, 2, 3, 4, 5, 6];
        let rows = vec![
            row(6, mac, SnmpValue::Int(8)),
            row(4, mac, SnmpValue::Bytes(b"ap\x00\xff-1\n".to_vec())),
        ];
        let inv = inventory(WlanFlavor::Huawei, &rows, &[], 1024);
        assert_eq!(inv.aps[0].name.as_deref(), Some("ap \u{fffd}-1"));
    }

    #[test]
    fn a_controller_over_its_cap_is_cut_and_says_so() {
        let rows: Vec<_> = (0u32..6)
            .map(|i| row(6, [0, 0, 0, 0, 0, i], SnmpValue::Int(8)))
            .collect();
        let inv = inventory(WlanFlavor::Huawei, &rows, &[], 4);
        assert_eq!(inv.aps.len(), 4);
        assert_eq!(inv.truncated_at, Some(6));
    }
}
