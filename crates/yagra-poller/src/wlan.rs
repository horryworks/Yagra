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
//! Two dialects: Huawei's HUAWEI-WLAN MIBs, and Cisco's AIRESPACE-WIRELESS-MIB, which AireOS and
//! the Catalyst 9800 both answer (ADR-064 増分 F). They share the MAC-indexed shape and the helpers;
//! what differs is decoded per dialect — the run-state words, which way round a radio's up/down
//! numbers run, where an SSID's name is (Huawei's index, Cisco's column — or, on a 9800, a second
//! table's), where an AP's clients come from (Huawei's AP table, Cisco's radios), and where the
//! controller's own totals come from (Huawei's scalars, Cisco's walks).
//!
//! ⚠️ **What a standby controller answers is not filtered here.** Its APs read `standby` with CPU,
//! memory and radio values of 0 — measured — and the inventory reports that faithfully as
//! [`WlanApState::Backup`]. Deciding that a backup's numbers are not readings needs every controller's
//! view of the AP at once, which only core has.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use yagra_bus::{RowName, Sample};
use yagra_common::{assign_radio_slots, WlanBand, WlanRadioObservation};
use yagra_common::{
    cisco_airespace_run_state, huawei_run_state, sanitize_wlan_text, ssid_row_key, ApMac,
    MetricKind, WlanApObservation, WlanFlavor, WlanInventory, MAX_APS_PER_CONTROLLER_HARD,
    METRIC_WLAN_SSID_AP_COUNT, METRIC_WLAN_SSID_CLIENTS, METRIC_WLAN_SSID_CLIENTS_2G4,
    METRIC_WLAN_SSID_CLIENTS_5G, METRIC_WLAN_SSID_CLIENTS_6G, METRIC_WLAN_SSID_IN_OCTETS,
    METRIC_WLAN_SSID_OUT_OCTETS,
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

/// Cisco `bsnAPEntry` columns that **must all answer** (ADR-064 増分 F, F3) — the ones the PoC's
/// AIR-CT3504 (AireOS 8.5.140.0) and the lab's Catalyst 9800 recording (IOS-XE 17.9.4) both
/// answered for every AP, and nothing else, for the reason [`HUAWEI_COLUMNS`] gives.
///
/// There is no client column in this table: an AP's client count is the sum of its radios'
/// (`bsnApIfNoOfUsers`, F6), so it needs the radio walk.
const CISCO_COLUMNS: [(u32, Field); 6] = [
    (6, Field::RunState),
    (3, Field::Name),
    (17, Field::Serial),
    (16, Field::Model),
    (8, Field::SwVersion),
    (19, Field::Ip),
];

/// Cisco columns read in the **second walk**, as full column OIDs — two of them are in
/// CISCO-LWAPP-AP-MIB's `cLApTable`, a different table indexed by the same base radio MAC, so a row
/// from either joins the same AP.
///
/// `.30` (AP group) is here and not in [`CISCO_COLUMNS`] because the 9800 recording has no such
/// column: in the required walk it would empty every 9800's AP list. The PoC's controller answers
/// all three (group `default-group`, CPU 0 %, memory 38 %).
const CISCO_OPTIONAL_COLUMNS: [(&str, Field); 3] = [
    ("1.3.6.1.4.1.14179.2.2.1.1.30", Field::Group),
    ("1.3.6.1.4.1.9.9.513.1.1.1.1.57", Field::Cpu),
    ("1.3.6.1.4.1.9.9.513.1.1.1.1.55", Field::Mem),
];

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
    required_fields(flavor)
        .into_iter()
        .map(|(oid, _)| oid)
        .collect()
}

/// The column OIDs of the second, optional walk ([`HUAWEI_OPTIONAL_COLUMNS`],
/// [`CISCO_OPTIONAL_COLUMNS`]).
#[must_use]
pub fn optional_columns(flavor: WlanFlavor) -> Vec<String> {
    optional_fields(flavor)
        .into_iter()
        .map(|(oid, _)| oid)
        .collect()
}

/// A dialect's required columns as `(full column OID, field)`, run state first.
fn required_fields(flavor: WlanFlavor) -> Vec<(String, Field)> {
    let under_root = |cols: &[(u32, Field)]| -> Vec<(String, Field)> {
        cols.iter()
            .map(|(n, f)| (format!("{}.{n}", flavor.root_oid()), *f))
            .collect()
    };
    match flavor {
        WlanFlavor::Huawei => under_root(&HUAWEI_COLUMNS),
        WlanFlavor::CiscoAirespace => under_root(&CISCO_COLUMNS),
    }
}

/// A dialect's optional columns as `(full column OID, field)`.
fn optional_fields(flavor: WlanFlavor) -> Vec<(String, Field)> {
    match flavor {
        WlanFlavor::Huawei => HUAWEI_OPTIONAL_COLUMNS
            .iter()
            .map(|(n, f)| (format!("{}.{n}", flavor.root_oid()), *f))
            .collect(),
        WlanFlavor::CiscoAirespace => CISCO_OPTIONAL_COLUMNS
            .iter()
            .map(|(oid, f)| ((*oid).to_owned(), *f))
            .collect(),
    }
}

/// What a dialect's run-state column value means, as `(the controller's word, state)`.
fn run_state(flavor: WlanFlavor, value: i64) -> (String, yagra_common::WlanApState) {
    match flavor {
        WlanFlavor::Huawei => huawei_run_state(value),
        WlanFlavor::CiscoAirespace => cisco_airespace_run_state(value),
    }
}

/// Whether this dialect's controller counts its own AP and client totals out of the walk, rather
/// than from scalars a template of its own reads (ADR-064 増分 F, F8). A Huawei AC has those scalars
/// (`T_HUAWEI_WLAN_CTL`); a Cisco controller has none that both AireOS and the 9800 answer — and
/// publishing the same metric from two sources on one node would draw both.
#[must_use]
pub fn counts_controller_totals(flavor: WlanFlavor) -> bool {
    match flavor {
        WlanFlavor::Huawei => false,
        WlanFlavor::CiscoAirespace => true,
    }
}

/// The scalars that say how many APs the controller's **platform** supports (ADR-064 増分 H, H5),
/// in the order they are preferred. Empty for a dialect that says it elsewhere.
///
/// Cisco keeps it in two places and each platform answers only one: AireOS the AIRESPACE-SWITCHING
/// `agentInventoryMaxNumberOfAPsSupported` (150 on the PoC's AIR-CT3504), the 9800 CISCO-LWAPP-AP's
/// `cLApGlobalMaxApsSupported` (250 on the lab's recording). One template item cannot hold two OIDs,
/// so both are asked in one GET and whichever answers is the reading. A Huawei AC's licence is a
/// different number with a different name, read by its own template.
#[must_use]
pub fn capacity_oids(flavor: WlanFlavor) -> &'static [&'static str] {
    match flavor {
        WlanFlavor::Huawei => &[],
        WlanFlavor::CiscoAirespace => &[
            "1.3.6.1.4.1.14179.1.1.1.18.0",
            "1.3.6.1.4.1.9.9.513.1.3.28.0",
        ],
    }
}

/// The platform's AP capacity among a GET's answers: the first of [`capacity_oids`] that answered
/// a real number. 0 is refused — a controller that supports no APs is not a controller, so a 0 is
/// an unimplemented object answering its default, and it would draw a capacity every AP exceeds.
#[must_use]
pub fn ap_capacity(flavor: WlanFlavor, answered: &[yagra_transport::SnmpSample]) -> Option<u32> {
    capacity_oids(flavor).iter().find_map(|oid| {
        answered
            .iter()
            .find(|s| s.oid.trim_start_matches('.') == *oid)
            .map(|s| s.value)
            .filter(|v| v.is_finite() && *v >= 1.0 && *v <= f64::from(u32::MAX))
            .map(|v| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let n = v as u32;
                n
            })
    })
}

/// How many APs the required walk's rows say are associated — the controller's joined count, taken
/// **before** the inventory is cut to its cap, so a controller over the cap still reports how many it
/// has (F8). Rows whose index is not a MAC are not APs and are not counted.
#[must_use]
pub fn joined_count(flavor: WlanFlavor, rows: &[SnmpInstanceRow]) -> usize {
    let Some((run_state_oid, _)) = required_fields(flavor)
        .into_iter()
        .find(|(_, f)| *f == Field::RunState)
    else {
        return 0;
    };
    let mut joined: std::collections::BTreeSet<ApMac> = std::collections::BTreeSet::new();
    for row in rows {
        if row.oid_base.trim_start_matches('.') != run_state_oid {
            continue;
        }
        let (Some(mac), SnmpValue::Int(v)) = (ApMac::from_subids(&row.instance), &row.value) else {
            continue;
        };
        if run_state(flavor, *v).1 == yagra_common::WlanApState::Associated {
            joined.insert(mac);
        }
    }
    joined.len()
}

/// The inventory in a dialect's rows, bounded to `max_aps` and the byte budget
/// ([`WlanInventory::bounded`]).
///
/// `optional` and `radio_rows` carry the second and third walks' rows and may each be empty —
/// those walks are allowed to fail, and an empty slice is exactly what "it did" looks like here.
/// All three are folded in **before** bounding, so the byte budget measures the observations that
/// will actually be sent — which is why the radios are attached here and not by the caller.
#[must_use]
pub fn inventory(
    flavor: WlanFlavor,
    rows: &[SnmpInstanceRow],
    optional: &[SnmpInstanceRow],
    radio_rows: &[SnmpInstanceRow],
    max_aps: u32,
) -> WlanInventory {
    let mut aps = observations(flavor, rows, optional);
    let mut by_ap = radios(flavor, radio_rows);
    for ap in &mut aps {
        ap.radios = by_ap.remove(&ap.mac).unwrap_or_default();
        match flavor {
            // The AP table has its own client column.
            WlanFlavor::Huawei => {}
            // It has none: the AP's clients are its radios' (F6). No radio answered ⇒ no count,
            // never a 0 that would read as an empty AP.
            WlanFlavor::CiscoAirespace => {
                ap.clients = ap
                    .radios
                    .iter()
                    .filter_map(|r| r.clients)
                    .reduce(u32::saturating_add);
            }
        }
    }
    WlanInventory::bounded(flavor, aps, max_aps)
}

fn observations(
    flavor: WlanFlavor,
    rows: &[SnmpInstanceRow],
    optional: &[SnmpInstanceRow],
) -> Vec<WlanApObservation> {
    let field_of: BTreeMap<String, Field> = required_fields(flavor)
        .into_iter()
        .chain(optional_fields(flavor))
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
                states.insert(mac, run_state(flavor, v));
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
                radios: Vec::new(),
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

// ─── Radios (ADR-064 増分 C) ───────────────────────────────────────────────────

/// Huawei `hwWlanRadioInfoEntry` columns read, as `(column number, field)`.
///
/// Left out, each for a reason: `.19` (channel bandwidth, measured 20 everywhere) is **not** a
/// line rate and must never reach `if_speed` — a 20 there would make a utilisation percentage out
/// of 20 bits per second; `.26` (idle ratio) is `100 - .25` on every measured row; `.23` (packet
/// error rate) and the frame counters read 0 on every row of the measured controller, so there is
/// nothing to say about how they behave; `.46` (maximum power) is a platform constant.
const HUAWEI_RADIO_COLUMNS: [(u32, RadioField); 11] = [
    (5, RadioField::Band),
    (6, RadioField::RunState),
    (7, RadioField::Channel),
    (24, RadioField::Noise),
    (25, RadioField::ChannelUtil),
    (29, RadioField::Interference),
    (40, RadioField::Clients),
    (41, RadioField::ClientSignal),
    (45, RadioField::TxPower),
    (31, RadioField::InOctets),
    (36, RadioField::OutOctets),
];

/// What a radio column contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RadioField {
    Band,
    RunState,
    Channel,
    Noise,
    ChannelUtil,
    Interference,
    Clients,
    ClientSignal,
    TxPower,
    InOctets,
    OutOctets,
}

/// The most radios one AP may report. Three bands, plus room for a second radio in each.
const RADIOS_PER_AP_MAX: usize = 6;

/// Huawei `hwWlanRadioActualEIRP` when the controller has no figure: the MIB says so outright
/// (`Unsigned32 (1..127 | 255)`).
const HUAWEI_NO_TX_POWER: i64 = 255;

/// Cisco radio columns read, as `(full column OID, field)` (ADR-064 増分 F, F5). The first four are
/// `bsnAPIfEntry`; channel utilization is `bsnAPIfLoadParametersEntry`, a separate table indexed by
/// the same (base radio MAC, slot), so its rows join the same radio.
///
/// Left out: `.6 bsnAPIfPhyTxPowerLevel` is a power **step** (1–8), not dBm, and must not reach
/// `wlan_radio_tx_power_dbm`; the table has no noise, interference, signal or byte column at all.
const CISCO_RADIO_COLUMNS: [(&str, CiscoRadioField); 5] = [
    ("1.3.6.1.4.1.14179.2.2.2.1.2", CiscoRadioField::Type),
    ("1.3.6.1.4.1.14179.2.2.2.1.4", CiscoRadioField::Channel),
    ("1.3.6.1.4.1.14179.2.2.2.1.12", CiscoRadioField::OperStatus),
    ("1.3.6.1.4.1.14179.2.2.2.1.15", CiscoRadioField::Clients),
    ("1.3.6.1.4.1.14179.2.2.13.1.3", CiscoRadioField::ChannelUtil),
];

/// What a Cisco radio column contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CiscoRadioField {
    /// `bsnAPIfType` — with the channel, it decides the band ([`WlanBand::from_cisco_airespace`]).
    Type,
    Channel,
    /// `bsnAPIfOperStatus` — down(1)/up(2), **the reverse of Huawei's numbering**.
    OperStatus,
    /// `bsnApIfNoOfUsers` — typed `Counter32` in the MIB, but it is how many are associated now.
    Clients,
    ChannelUtil,
}

/// The radio column OIDs to walk for a dialect.
#[must_use]
pub fn radio_columns(flavor: WlanFlavor) -> Vec<String> {
    match flavor {
        WlanFlavor::Huawei => HUAWEI_RADIO_COLUMNS
            .iter()
            .map(|(n, _)| format!("{}.{n}", flavor.radio_root_oid()))
            .collect(),
        WlanFlavor::CiscoAirespace => CISCO_RADIO_COLUMNS
            .iter()
            .map(|(oid, _)| (*oid).to_owned())
            .collect(),
    }
}

/// The most rows the radio walk takes, across every column.
#[must_use]
pub fn radio_walk_row_budget(flavor: WlanFlavor) -> usize {
    radio_columns(flavor).len()
        * usize::try_from(MAX_APS_PER_CONTROLLER_HARD).unwrap_or(usize::MAX)
        * RADIOS_PER_AP_MAX
}

/// The radios in a dialect's rows, grouped by the AP they belong to and numbered into slots.
///
/// 🚨 **A radio whose band the controller did not name is dropped.** The band decides the slot,
/// and a guessed slot is a series attributed to the wrong radio — which looks like a working
/// reading rather than a missing one.
#[must_use]
pub fn radios(
    flavor: WlanFlavor,
    rows: &[SnmpInstanceRow],
) -> BTreeMap<ApMac, Vec<WlanRadioObservation>> {
    match flavor {
        WlanFlavor::Huawei => huawei_radios(flavor, rows),
        WlanFlavor::CiscoAirespace => cisco_radios(rows),
    }
}

/// The controller's clients on each band, summed over every radio the radio walk returned
/// (ADR-064 増分 H, H4) — all of them, before the inventory is cut to its cap, as [`joined_count`]
/// counts.
///
/// Every band is present, 0 where no radio of that band carried a client, so a controller with no
/// 6 GHz radio reads 0 on 6 GHz rather than a gap — the same statement a Huawei AC's own scalar
/// makes. `None` when radios were read and **not one** answered a client count: a column the
/// controller does not implement is no reading, never "nobody is connected".
///
/// ⚠️ A radio whose band could not be decided is left out (as [`radios`] drops it), and so are an
/// AP's radios past [`RADIOS_PER_AP_MAX`], so the three need not add up to the SSID table's total.
/// On the lab's 9800 recording they do not: 18 + 20 + 0 against 41.
#[must_use]
pub fn clients_per_band(
    flavor: WlanFlavor,
    radio_rows: &[SnmpInstanceRow],
) -> Option<BTreeMap<WlanBand, u32>> {
    let mut per_band: BTreeMap<WlanBand, u32> = WlanBand::ALL.iter().map(|b| (*b, 0)).collect();
    let mut answered = false;
    for radio in radios(flavor, radio_rows).values().flatten() {
        if let Some(n) = radio.clients {
            answered = true;
            let total = per_band.entry(radio.band).or_insert(0);
            *total = total.saturating_add(n);
        }
    }
    answered.then_some(per_band)
}

fn cisco_radios(rows: &[SnmpInstanceRow]) -> BTreeMap<ApMac, Vec<WlanRadioObservation>> {
    let field_of: BTreeMap<&str, CiscoRadioField> = CISCO_RADIO_COLUMNS.iter().copied().collect();
    let mut cells: BTreeMap<(ApMac, u32), Vec<(CiscoRadioField, &SnmpValue)>> = BTreeMap::new();
    for row in rows {
        let Some(field) = field_of.get(row.oid_base.trim_start_matches('.')) else {
            continue;
        };
        let Some((mac, slot)) = radio_index(&row.instance) else {
            continue;
        };
        cells
            .entry((mac, slot))
            .or_default()
            .push((*field, &row.value));
    }
    let mut by_ap: BTreeMap<ApMac, Vec<(u32, WlanRadioObservation)>> = BTreeMap::new();
    for ((mac, slot), fields) in cells {
        let int = |wanted: CiscoRadioField| {
            fields.iter().find_map(|(f, v)| match (f, v) {
                (f, SnmpValue::Int(n)) if *f == wanted => Some(*n),
                _ => None,
            })
        };
        let channel = int(CiscoRadioField::Channel).and_then(|n| u32::try_from(n).ok());
        let Some(band) = WlanBand::from_cisco_airespace(int(CiscoRadioField::Type), channel) else {
            continue;
        };
        let r = WlanRadioObservation {
            // Replaced by `assign_radio_slots`; a radio is never published with this.
            slot: 0,
            band,
            // down(1)/up(2); anything else is not a state.
            up: match int(CiscoRadioField::OperStatus) {
                Some(2) => Some(true),
                Some(1) => Some(false),
                _ => None,
            },
            clients: int(CiscoRadioField::Clients).and_then(|n| u32::try_from(n).ok()),
            channel,
            channel_util_pct: int(CiscoRadioField::ChannelUtil).and_then(|n| u32::try_from(n).ok()),
            interference_pct: None,
            noise_dbm: None,
            client_signal_dbm: None,
            tx_power_dbm: None,
            in_octets: None,
            out_octets: None,
        };
        let ap = by_ap.entry(mac).or_default();
        if ap.len() < RADIOS_PER_AP_MAX {
            ap.push((slot, r));
        }
    }
    by_ap
        .into_iter()
        .map(|(mac, radios)| (mac, assign_radio_slots(radios)))
        .collect()
}

fn huawei_radios(
    flavor: WlanFlavor,
    rows: &[SnmpInstanceRow],
) -> BTreeMap<ApMac, Vec<WlanRadioObservation>> {
    let field_of: BTreeMap<String, RadioField> = HUAWEI_RADIO_COLUMNS
        .iter()
        .map(|(n, f)| (format!("{}.{n}", flavor.radio_root_oid()), *f))
        .collect();
    // Keyed by (AP, the vendor's radio id) — the index's first six sub-identifiers are the AP
    // table's own index, which is what attaches a radio to an AP without a second lookup.
    let mut cells: BTreeMap<(ApMac, u32), Vec<(RadioField, &SnmpValue)>> = BTreeMap::new();
    for row in rows {
        let Some(field) = field_of.get(row.oid_base.trim_start_matches('.')) else {
            continue;
        };
        let Some((mac, radio_id)) = radio_index(&row.instance) else {
            continue;
        };
        cells
            .entry((mac, radio_id))
            .or_default()
            .push((*field, &row.value));
    }
    let mut by_ap: BTreeMap<ApMac, Vec<(u32, WlanRadioObservation)>> = BTreeMap::new();
    for ((mac, radio_id), fields) in cells {
        let band = fields.iter().find_map(|(f, v)| match (f, v) {
            (RadioField::Band, SnmpValue::Int(n)) => WlanBand::from_huawei(*n),
            _ => None,
        });
        let Some(band) = band else { continue };
        let mut r = WlanRadioObservation {
            // Replaced by `assign_radio_slots`; a radio is never published with this.
            slot: 0,
            band,
            up: None,
            clients: None,
            channel: None,
            channel_util_pct: None,
            interference_pct: None,
            noise_dbm: None,
            client_signal_dbm: None,
            tx_power_dbm: None,
            in_octets: None,
            out_octets: None,
        };
        for (field, value) in fields {
            match field {
                RadioField::Band => {}
                // up(1)/down(2)/invalid(255): the third is not a state, so it stays `None`.
                RadioField::RunState => {
                    r.up = match value {
                        SnmpValue::Int(1) => Some(true),
                        SnmpValue::Int(2) => Some(false),
                        _ => None,
                    };
                }
                RadioField::Channel => r.channel = non_negative(value),
                // 0 is the MIB's own "invalid", and a noise floor of 0 dBm would read as a radio
                // being drowned rather than as a number the controller does not have.
                RadioField::Noise => r.noise_dbm = signed_nonzero(value),
                RadioField::ChannelUtil => r.channel_util_pct = non_negative(value),
                RadioField::Interference => r.interference_pct = non_negative(value),
                RadioField::Clients => r.clients = non_negative(value),
                // 0 here means "no clients to average", not "0 dBm" (which would be a signal
                // stronger than any real one).
                RadioField::ClientSignal => r.client_signal_dbm = signed_nonzero(value),
                RadioField::TxPower => {
                    r.tx_power_dbm = match value {
                        SnmpValue::Int(HUAWEI_NO_TX_POWER) => None,
                        SnmpValue::Int(v) => i32::try_from(*v).ok(),
                        SnmpValue::Bytes(_) | SnmpValue::Oid(_) => None,
                    };
                }
                RadioField::InOctets => r.in_octets = counter64(value),
                RadioField::OutOctets => r.out_octets = counter64(value),
            }
        }
        let ap = by_ap.entry(mac).or_default();
        if ap.len() < RADIOS_PER_AP_MAX {
            ap.push((radio_id, r));
        }
    }
    by_ap
        .into_iter()
        .map(|(mac, radios)| (mac, assign_radio_slots(radios)))
        .collect()
}

/// The AP and radio a radio row's index names: six sub-identifiers of MAC, then the radio id.
fn radio_index(instance: &[u32]) -> Option<(ApMac, u32)> {
    let (mac, radio_id) = instance.split_at(instance.len().checked_sub(1)?);
    Some((ApMac::from_subids(mac)?, *radio_id.first()?))
}

/// A signed reading whose dialect spells "no reading" as zero.
fn signed_nonzero(value: &SnmpValue) -> Option<i32> {
    match value {
        SnmpValue::Int(0) => None,
        SnmpValue::Int(v) => i32::try_from(*v).ok(),
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
    /// Clients the controller counts with no band split — a Cisco controller's
    /// `bsnDot11EssNumberOfMobileStations` (ADR-064 増分 F, F7). `None` on a dialect that splits.
    pub clients_unsplit: Option<u32>,
    pub clients_2g4: Option<u32>,
    pub clients_5g: Option<u32>,
    pub clients_6g: Option<u32>,
    pub in_octets: Option<u64>,
    pub out_octets: Option<u64>,
}

impl WlanSsidReading {
    /// Clients on the SSID: the controller's own unsplit count when it keeps one, otherwise the sum
    /// over every band it answered for, or `None` when it answered for none.
    ///
    /// Summing only what was answered is deliberate: a controller with no 6 GHz radios omits that
    /// column, and treating the absence as zero would be right by luck rather than by evidence.
    #[must_use]
    pub fn clients(&self) -> Option<u32> {
        if self.clients_unsplit.is_some() {
            return self.clients_unsplit;
        }
        let parts = [self.clients_2g4, self.clients_5g, self.clients_6g];
        parts
            .iter()
            .any(Option::is_some)
            .then(|| parts.iter().flatten().sum())
    }
}

/// Cisco SSID columns read, as `(full column OID, field)` (ADR-064 増分 F, F7, and 増分 H, H1): the
/// SSID's name and the clients on it from `bsnDot11EssEntry` — its index is a WLAN number — and a
/// second name from CISCO-LWAPP-WLAN-MIB's `cLWlanSsid`, indexed by the same WLAN number. The
/// table has no band split, AP count or bytes.
///
/// 🚨 **The second name is what the 9800 has.** The lab's 9800 recording answers `.38` for all eight
/// of its WLANs and `.2` for none, so with `.2` alone it had no SSIDs, an SSID count of 0 and no
/// controller client count — and the check that shipped 増分 F looked only at the AP list. AireOS
/// answers both, identically (the PoC's `office24` / `office5`). A column a controller does not
/// implement is still an answered column, so asking for both never costs a complete walk.
const CISCO_SSID_COLUMNS: [(&str, CiscoSsidField); 3] = [
    ("1.3.6.1.4.1.14179.2.1.1.1.2", CiscoSsidField::Name),
    ("1.3.6.1.4.1.14179.2.1.1.1.38", CiscoSsidField::Clients),
    ("1.3.6.1.4.1.9.9.512.1.1.1.1.4", CiscoSsidField::WlanName),
];

/// What a Cisco SSID column contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CiscoSsidField {
    /// `bsnDot11EssSsid`, preferred when it answers.
    Name,
    /// `bsnDot11EssNumberOfMobileStations` — `Counter32` in the MIB, a current count in practice.
    Clients,
    /// `cLWlanSsid` — the name when `.2` does not answer.
    WlanName,
}

/// The SSID column OIDs to walk for a dialect.
#[must_use]
pub fn ssid_columns(flavor: WlanFlavor) -> Vec<String> {
    match flavor {
        WlanFlavor::Huawei => HUAWEI_SSID_COLUMNS
            .iter()
            .map(|(n, _)| format!("{}.{n}", flavor.ssid_root_oid()))
            .collect(),
        WlanFlavor::CiscoAirespace => CISCO_SSID_COLUMNS
            .iter()
            .map(|(oid, _)| (*oid).to_owned())
            .collect(),
    }
}

/// The most rows the SSID walk takes, across every column.
#[must_use]
pub fn ssid_walk_row_budget(flavor: WlanFlavor) -> usize {
    ssid_columns(flavor).len() * WLAN_SSID_MAX * 2
}

/// One controller's SSID table: the SSIDs, and what only the whole table can say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsidTable {
    /// The SSIDs, in row-key order and bounded to [`WLAN_SSID_MAX`].
    pub readings: Vec<WlanSsidReading>,
    /// The clients the table carries altogether, or `None` when no row answered a count.
    ///
    /// Cisco: the sum over **every WLAN row**, named or not (ADR-064 増分 H, H2) — a client on a
    /// WLAN whose name could not be read is still a client of this controller. Huawei: the sum over
    /// the SSIDs, whose names are their index and so never missing.
    pub clients_total: Option<u32>,
    /// WLAN rows that answered with no name from either column. Their clients are in
    /// [`Self::clients_total`], but they are no SSID anyone can find — and while there is one, how
    /// many SSIDs there are is not known (H3).
    pub unnamed_rows: usize,
}

/// The SSID table in a dialect's rows.
///
/// 🚨 **A row key collision drops the row rather than merging it.** Two SSIDs whose names hash the
/// same would otherwise share one series, and the result would look like one SSID with somebody
/// else's client count — a wrong number, where a missing one is only a gap.
#[must_use]
pub fn ssid_table(flavor: WlanFlavor, rows: &[SnmpInstanceRow]) -> SsidTable {
    match flavor {
        WlanFlavor::Huawei => {
            let readings = huawei_ssids(flavor, rows);
            let clients_total = readings
                .iter()
                .filter_map(WlanSsidReading::clients)
                .reduce(u32::saturating_add);
            SsidTable {
                readings,
                clients_total,
                unnamed_rows: 0,
            }
        }
        WlanFlavor::CiscoAirespace => cisco_ssid_table(rows),
    }
}

/// A Cisco controller's SSIDs. The name comes from column `.2`, or from `cLWlanSsid` when `.2` did
/// not answer, joined to its WLAN number (H1); two WLANs broadcasting the **same** SSID are one SSID
/// here, their clients added: an operator asks how many are on `guest`, not on WLAN 3. Two
/// **different** names whose row keys collide are still dropped, as [`ssid_table`] says.
///
/// ⚠️ The WLAN set is the one the SSID table names — rows seen in `.2` or `.38`. A `cLWlanSsid` row
/// for a WLAN number the table does not have is ignored: the two tables are assumed to number
/// WLANs alike (measured on 2 AireOS WLANs and the 9800 recording's 8, never on a real 9800), and a
/// row that fits nothing is the first sign that assumption does not hold. So is a WLAN whose two
/// names differ, which keeps `.2` and is counted in `yagra_wlan_ssid_name_mismatch_total`.
fn cisco_ssid_table(rows: &[SnmpInstanceRow]) -> SsidTable {
    let field_of: BTreeMap<&str, CiscoSsidField> = CISCO_SSID_COLUMNS.iter().copied().collect();
    let mut names: BTreeMap<Vec<u32>, String> = BTreeMap::new();
    let mut wlan_names: BTreeMap<Vec<u32>, String> = BTreeMap::new();
    let mut clients: BTreeMap<Vec<u32>, u32> = BTreeMap::new();
    let mut wlans: std::collections::BTreeSet<Vec<u32>> = std::collections::BTreeSet::new();
    for row in rows {
        match field_of.get(row.oid_base.trim_start_matches('.')) {
            Some(CiscoSsidField::Name) => {
                wlans.insert(row.instance.clone());
                if let Some(name) = text(&row.value) {
                    names.insert(row.instance.clone(), name);
                }
            }
            Some(CiscoSsidField::Clients) => {
                wlans.insert(row.instance.clone());
                if let Some(n) = non_negative(&row.value) {
                    clients.insert(row.instance.clone(), n);
                }
            }
            Some(CiscoSsidField::WlanName) => {
                if let Some(name) = text(&row.value) {
                    wlan_names.insert(row.instance.clone(), name);
                }
            }
            None => {}
        }
    }
    let clients_total = wlans
        .iter()
        .filter_map(|w| clients.get(w).copied())
        .reduce(u32::saturating_add);
    let mut unnamed_rows = 0;
    let mut by_name: BTreeMap<String, Option<u32>> = BTreeMap::new();
    for wlan in &wlans {
        let name = match (names.get(wlan), wlan_names.get(wlan)) {
            (Some(ess), Some(other)) => {
                if ess != other {
                    tracing::debug!(
                        ssid = %ess,
                        wlan_ssid = %other,
                        "the two Cisco SSID tables name one WLAN differently; keeping the first"
                    );
                    metrics::counter!("yagra_wlan_ssid_name_mismatch_total").increment(1);
                }
                ess
            }
            (Some(ess), None) => ess,
            (None, Some(other)) => other,
            // No SSID anyone can find; its clients are in the total and nowhere else.
            (None, None) => {
                unnamed_rows += 1;
                continue;
            }
        };
        let entry = by_name.entry(name.clone()).or_insert(None);
        if let Some(n) = clients.get(wlan) {
            *entry = Some(entry.unwrap_or(0).saturating_add(*n));
        }
    }
    let mut out: Vec<WlanSsidReading> = Vec::new();
    let mut seen: BTreeMap<u32, String> = BTreeMap::new();
    for (name, clients_unsplit) in by_name {
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
        out.push(WlanSsidReading {
            name,
            row,
            ap_count: None,
            clients_unsplit,
            clients_2g4: None,
            clients_5g: None,
            clients_6g: None,
            in_octets: None,
            out_octets: None,
        });
    }
    out.sort_by_key(|r| r.row);
    out.truncate(WLAN_SSID_MAX);
    SsidTable {
        readings: out,
        clients_total,
        unnamed_rows,
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
            clients_unsplit: None,
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
    const RADIO_ROOT: &str = "1.3.6.1.4.1.2011.6.139.16.1.2.1";

    /// One radio column cell: the index is the AP MAC then the vendor radio id.
    fn radio_cell(column: u32, mac: [u32; 6], radio_id: u32, v: i64) -> SnmpInstanceRow {
        let mut instance = mac.to_vec();
        instance.push(radio_id);
        SnmpInstanceRow {
            oid_base: format!("{RADIO_ROOT}.{column}"),
            instance,
            value: SnmpValue::Int(v),
        }
    }

    /// The measured shape: two radios per AP, radio 0 on 2.4 GHz and radio 1 on 5 GHz.
    #[test]
    fn radios_are_numbered_by_band_and_attached_to_their_ap() {
        let up = [84, 246, 226, 10, 2, 128];
        let rows = vec![
            radio_cell(5, up, 0, 1),
            radio_cell(7, up, 0, 11),
            radio_cell(40, up, 0, 3),
            radio_cell(5, up, 1, 2),
            radio_cell(7, up, 1, 44),
            radio_cell(40, up, 1, 12),
        ];
        let by_ap = radios(WlanFlavor::Huawei, &rows);
        let mine = by_ap
            .get(&ApMac::new([84, 246, 226, 10, 2, 128]))
            .expect("the radios are filed under the AP the index names");
        assert_eq!(mine.len(), 2);
        assert_eq!(mine[0].band, yagra_common::WlanBand::Band2G4);
        assert_eq!(mine[0].slot, 1, "2.4 GHz is slot 1");
        assert_eq!(mine[0].channel, Some(11));
        assert_eq!(mine[0].clients, Some(3));
        assert_eq!(mine[1].band, yagra_common::WlanBand::Band5G);
        assert_eq!(mine[1].slot, 2, "5 GHz is slot 2");
        assert_eq!(mine[1].clients, Some(12));
    }

    /// A second radio in one band takes the band base plus the stride, so a band always owns its
    /// last digit and a future band cannot collide with it (ADR-064 R6).
    #[test]
    fn a_second_radio_in_one_band_is_offset_by_the_stride() {
        let mac = [1, 2, 3, 4, 5, 6];
        let rows = vec![
            radio_cell(5, mac, 0, 2),
            radio_cell(5, mac, 1, 2),
            radio_cell(5, mac, 2, 3),
        ];
        let by_ap = radios(WlanFlavor::Huawei, &rows);
        let slots: Vec<u32> = by_ap[&ApMac::new([1, 2, 3, 4, 5, 6])]
            .iter()
            .map(|r| r.slot)
            .collect();
        assert_eq!(slots, vec![2, 12, 3]);
    }

    /// 🚨 The dialect spells three different "no reading" values three different ways, and each
    /// one would be a believable measurement if it were stored: a 0 dBm noise floor reads as a
    /// radio being drowned, a 0 dBm client signal as the strongest possible, and 255 dBm of
    /// transmit power as nothing at all.
    #[test]
    fn the_dialects_invalid_markers_are_not_readings() {
        let mac = [9, 9, 9, 9, 9, 9];
        let rows = vec![
            radio_cell(5, mac, 0, 2),
            radio_cell(24, mac, 0, 0),
            radio_cell(41, mac, 0, 0),
            radio_cell(45, mac, 0, 255),
        ];
        let by_ap = radios(WlanFlavor::Huawei, &rows);
        let r = &by_ap[&ApMac::new([9, 9, 9, 9, 9, 9])][0];
        assert_eq!(r.noise_dbm, None);
        assert_eq!(r.client_signal_dbm, None);
        assert_eq!(r.tx_power_dbm, None);

        // …and a real reading of each is kept, so the check above is not "everything is dropped".
        let rows = vec![
            radio_cell(5, mac, 0, 2),
            radio_cell(24, mac, 0, -96),
            radio_cell(41, mac, 0, -77),
            radio_cell(45, mac, 0, 23),
        ];
        let by_ap = radios(WlanFlavor::Huawei, &rows);
        let r = &by_ap[&ApMac::new([9, 9, 9, 9, 9, 9])][0];
        assert_eq!(r.noise_dbm, Some(-96));
        assert_eq!(r.client_signal_dbm, Some(-77));
        assert_eq!(r.tx_power_dbm, Some(23));
    }

    /// A radio whose band the controller did not name gets no slot, because a guessed slot is a
    /// series attributed to the wrong radio — which looks like a reading rather than a gap.
    #[test]
    fn a_radio_with_no_band_is_dropped() {
        let mac = [7, 7, 7, 7, 7, 7];
        let rows = vec![radio_cell(7, mac, 0, 36), radio_cell(40, mac, 0, 5)];
        assert!(radios(WlanFlavor::Huawei, &rows).is_empty());
    }

    /// The radios reach the inventory attached to the right AP, which is the join the whole
    /// increment rests on: the radio index's first six sub-identifiers are the AP table's index.
    #[test]
    fn the_inventory_carries_each_aps_own_radios() {
        let up = [84, 246, 226, 10, 2, 128];
        let radio_rows = vec![radio_cell(5, up, 0, 1), radio_cell(40, up, 0, 4)];
        let inv = inventory(
            WlanFlavor::Huawei,
            &active_controller_rows(),
            &[],
            &radio_rows,
            1024,
        );
        let with_radio: Vec<&str> = inv
            .aps
            .iter()
            .filter(|a| !a.radios.is_empty())
            .filter_map(|a| a.name.as_deref())
            .collect();
        assert_eq!(with_radio, vec!["site-ap-001"]);
        let ap = inv
            .aps
            .iter()
            .find(|a| a.name.as_deref() == Some("site-ap-001"))
            .unwrap();
        assert_eq!(ap.radios[0].clients, Some(4));
        assert_eq!(ap.radios[0].slot, 1);
    }

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
        let up = [84, 246, 226, 10, 2, 128];
        let warm = [96, 16, 158, 10, 5, 96];
        let optional = vec![
            row(83, up, SnmpValue::Int(66)),
            row(80, up, SnmpValue::Int(1)),
            row(83, warm, SnmpValue::Int(255)),
        ];
        let inv = inventory(
            WlanFlavor::Huawei,
            &active_controller_rows(),
            &optional,
            &[],
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
            // yagraguest: 2 on 2.4 GHz, 11 on 5 GHz, broadcast by 30 APs.
            cell(2, "yagraguest", 2),
            cell(3, "yagraguest", 11),
            cell(17, "yagraguest", 0),
            cell(4, "yagraguest", 30),
            cell(5, "yagraguest", 283_912_285_339),
            cell(9, "yagraguest", 214_712_781_505_206),
            // office5: 2 clients, all on 5 GHz.
            cell(2, "office5", 0),
            cell(3, "office5", 2),
            cell(4, "office5", 30),
            // office24 and yagra-biz: broadcast by every AP, nobody on them.
            cell(2, "office24", 0),
            cell(3, "office24", 0),
            cell(4, "office24", 30),
            cell(2, "yagra-biz", 0),
            cell(3, "yagra-biz", 0),
            cell(4, "yagra-biz", 30),
        ]
    }

    #[test]
    fn the_ssid_table_reads_into_one_reading_per_ssid_with_its_name() {
        let ssids = ssid_table(WlanFlavor::Huawei, &ssid_rows()).readings;
        let names: Vec<&str> = ssids.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names.len(), 4, "{names:?}");
        let find = |n: &str| {
            ssids
                .iter()
                .find(|r| r.name == n)
                .unwrap_or_else(|| panic!("{n} decoded"))
        };
        assert_eq!(find("yagraguest").clients_2g4, Some(2));
        assert_eq!(find("yagraguest").clients_5g, Some(11));
        assert_eq!(find("yagraguest").clients(), Some(13));
        assert_eq!(find("yagraguest").in_octets, Some(283_912_285_339));
        assert_eq!(find("office5").clients(), Some(2));
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
        let ssids = ssid_table(WlanFlavor::Huawei, &ssid_rows()).readings;
        let (samples, names) = ssid_samples(&ssids);
        let empty = ssids
            .iter()
            .find(|r| r.name == "office24")
            .expect("the empty SSID is in the readings");
        assert_eq!(empty.clients(), Some(0));
        assert!(
            names
                .iter()
                .any(|n| n.row == empty.row && n.name == "office24"),
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
        assert!(ssid_table(WlanFlavor::Huawei, &[row]).readings.is_empty());
    }

    /// The row key is a fact stored in the TSDB and joined to a name in PostgreSQL: changing it
    /// orphans both. Pinned to literals for the same reason [`ap_id`] is.
    #[test]
    fn an_ssid_row_key_is_pinned_to_its_name() {
        assert_eq!(yagra_common::ssid_row_key("yagraguest"), 34_426_187);
        assert_eq!(yagra_common::ssid_row_key("office5"), 2_192_495_692);
        assert_ne!(
            yagra_common::ssid_row_key("office5"),
            yagra_common::ssid_row_key("office24")
        );
    }
    fn active_controller_rows() -> Vec<SnmpInstanceRow> {
        let up = [84, 246, 226, 10, 2, 128];
        let down = [96, 16, 158, 10, 3, 160];
        let warm = [96, 16, 158, 10, 5, 96];
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
        let inv = inventory(
            WlanFlavor::Huawei,
            &active_controller_rows(),
            &[],
            &[],
            1024,
        );
        assert_eq!(inv.aps.len(), 3);
        assert_eq!(inv.truncated_at, None);
        let up = &inv.aps[0];
        assert_eq!(up.mac.to_string(), "54:f6:e2:0a:02:80");
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
        let inv = inventory(
            WlanFlavor::Huawei,
            &active_controller_rows(),
            &[],
            &[],
            1024,
        );
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
        let mac = [84, 246, 226, 10, 2, 128];
        let rows = vec![
            row(6, mac, SnmpValue::Int(11)),
            row(44, mac, SnmpValue::Int(5)),
            row(41, mac, SnmpValue::Int(0)),
        ];
        let inv = inventory(WlanFlavor::Huawei, &rows, &[], &[], 1024);
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
        assert!(inventory(WlanFlavor::Huawei, &rows, &[], &[], 1024)
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
        assert!(inventory(WlanFlavor::Huawei, &rows, &[], &[], 1024)
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
        let inv = inventory(WlanFlavor::Huawei, &rows, &[], &[], 1024);
        assert_eq!(inv.aps[0].name.as_deref(), Some("ap \u{fffd}-1"));
    }

    #[test]
    fn a_controller_over_its_cap_is_cut_and_says_so() {
        let rows: Vec<_> = (0u32..6)
            .map(|i| row(6, [0, 0, 0, 0, 0, i], SnmpValue::Int(8)))
            .collect();
        let inv = inventory(WlanFlavor::Huawei, &rows, &[], &[], 4);
        assert_eq!(inv.aps.len(), 4);
        assert_eq!(inv.truncated_at, Some(6));
    }

    // ─── Cisco, AIRESPACE-WIRELESS-MIB (ADR-064 増分 F) ────────────────────────────────

    const CISCO_AP: &str = "1.3.6.1.4.1.14179.2.2.1.1";
    const CISCO_RADIO: &str = "1.3.6.1.4.1.14179.2.2.2.1";
    const CISCO_LOAD: &str = "1.3.6.1.4.1.14179.2.2.13.1";
    const CISCO_SSID: &str = "1.3.6.1.4.1.14179.2.1.1.1";
    const CLAP: &str = "1.3.6.1.4.1.9.9.513.1.1.1.1";
    /// Base radio MACs in the PoC's shape (`00:5d:73:…`, `70:6d:15:…`); the Ethernet MACs are
    /// other values, in a column this dialect does not read.
    const SERVING: [u32; 6] = [0, 93, 115, 10, 1, 224];
    const UPGRADING: [u32; 6] = [112, 109, 21, 10, 4, 192];

    fn at(table: &str, column: u32, instance: &[u32], value: SnmpValue) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: format!("{table}.{column}"),
            instance: instance.to_vec(),
            value,
        }
    }

    fn with_slot(mac: [u32; 6], slot: u32) -> Vec<u32> {
        let mut v = mac.to_vec();
        v.push(slot);
        v
    }

    /// The required columns for two APs, shaped like the PoC's controller: one serving, one
    /// downloading an image. Names, serials and addresses are made up.
    fn cisco_ap_rows() -> Vec<SnmpInstanceRow> {
        let mut rows = Vec::new();
        for (mac, name, state, serial, ip) in [
            (SERVING, "site2-ap01", 1, "FGL0000A0AB", [192, 0, 2, 11]),
            (UPGRADING, "site2-ap04", 3, "FGL0000A0AC", [192, 0, 2, 14]),
        ] {
            rows.push(at(CISCO_AP, 6, &mac, SnmpValue::Int(state)));
            rows.push(at(CISCO_AP, 3, &mac, bytes(name)));
            rows.push(at(CISCO_AP, 8, &mac, bytes("8.5.140.0")));
            rows.push(at(CISCO_AP, 16, &mac, bytes("AIR-AP2802I-Q-K9")));
            rows.push(at(CISCO_AP, 17, &mac, bytes(serial)));
            rows.push(at(CISCO_AP, 19, &mac, SnmpValue::Bytes(ip.to_vec())));
        }
        rows
    }

    /// The PoC's two radios per AP: slot 0 answers type `3` (not in the MIB) on channel 6, slot 1
    /// dot11a(2) on channel 132; both up(2). Channel utilization comes from the load table.
    fn cisco_radio_rows(mac: [u32; 6]) -> Vec<SnmpInstanceRow> {
        let s0 = with_slot(mac, 0);
        let s1 = with_slot(mac, 1);
        vec![
            at(CISCO_RADIO, 2, &s0, SnmpValue::Int(3)),
            at(CISCO_RADIO, 4, &s0, SnmpValue::Int(6)),
            at(CISCO_RADIO, 12, &s0, SnmpValue::Int(2)),
            at(CISCO_RADIO, 15, &s0, SnmpValue::Int(2)),
            at(CISCO_LOAD, 3, &s0, SnmpValue::Int(42)),
            at(CISCO_RADIO, 2, &s1, SnmpValue::Int(2)),
            at(CISCO_RADIO, 4, &s1, SnmpValue::Int(132)),
            at(CISCO_RADIO, 12, &s1, SnmpValue::Int(2)),
            at(CISCO_RADIO, 15, &s1, SnmpValue::Int(5)),
            at(CISCO_LOAD, 3, &s1, SnmpValue::Int(0)),
        ]
    }

    /// 🚨 The required set is what both OS families answered, and nothing more (F3). `.30` is not
    /// in it because the 9800 recording has no such column; the client count is not in it because
    /// the table has none.
    #[test]
    fn the_cisco_columns_are_the_ones_aireos_and_the_9800_both_answer() {
        let at_root = |n: u32| format!("{CISCO_AP}.{n}");
        assert_eq!(
            columns(WlanFlavor::CiscoAirespace),
            vec![
                at_root(6),
                at_root(3),
                at_root(17),
                at_root(16),
                at_root(8),
                at_root(19)
            ],
            "run state first, then the descriptive columns"
        );
        let optional = optional_columns(WlanFlavor::CiscoAirespace);
        assert_eq!(
            optional,
            vec![at_root(30), format!("{CLAP}.57"), format!("{CLAP}.55")]
        );
        for oid in &optional {
            assert!(!columns(WlanFlavor::CiscoAirespace).contains(oid), "{oid}");
        }
        assert!(counts_controller_totals(WlanFlavor::CiscoAirespace));
        assert!(!counts_controller_totals(WlanFlavor::Huawei));
    }

    #[test]
    fn a_cisco_controller_reads_into_one_observation_per_ap_keyed_by_its_radio_mac() {
        let optional = vec![
            at(CISCO_AP, 30, &SERVING, bytes("default-group")),
            at(CLAP, 57, &SERVING, SnmpValue::Int(0)),
            at(CLAP, 55, &SERVING, SnmpValue::Int(38)),
        ];
        let inv = inventory(
            WlanFlavor::CiscoAirespace,
            &cisco_ap_rows(),
            &optional,
            &cisco_radio_rows(SERVING),
            MAX_APS_PER_CONTROLLER_HARD,
        );
        assert_eq!(inv.flavor, WlanFlavor::CiscoAirespace);
        assert_eq!(inv.aps.len(), 2);
        let ap = &inv.aps[0];
        assert_eq!(ap.mac.to_string(), "00:5d:73:0a:01:e0");
        assert_eq!(ap.name.as_deref(), Some("site2-ap01"));
        assert_eq!(ap.model.as_deref(), Some("AIR-AP2802I-Q-K9"));
        assert_eq!(ap.serial.as_deref(), Some("FGL0000A0AB"));
        assert_eq!(ap.sw_version.as_deref(), Some("8.5.140.0"));
        assert_eq!(ap.ip, Some("192.0.2.11".parse().unwrap()));
        assert_eq!(ap.vendor_group.as_deref(), Some("default-group"));
        assert_eq!(
            (ap.run_state.as_str(), ap.state),
            ("associated", WlanApState::Associated)
        );
        assert_eq!((ap.cpu_pct, ap.mem_pct), (Some(0), Some(38)));
        // No client column: the AP's clients are its radios'.
        assert_eq!(ap.clients, Some(7));
        assert_eq!(ap.temp_c, None);
        let bands: Vec<(u32, yagra_common::WlanBand)> =
            ap.radios.iter().map(|r| (r.slot, r.band)).collect();
        assert_eq!(
            bands,
            vec![
                (1, yagra_common::WlanBand::Band2G4),
                (2, yagra_common::WlanBand::Band5G)
            ]
        );
        assert_eq!(ap.radios[0].channel_util_pct, Some(42));
        assert_eq!(ap.radios[0].up, Some(true));
        assert_eq!(ap.radios[1].channel, Some(132));
        // Nothing this dialect does not read is made up.
        assert!(ap
            .radios
            .iter()
            .all(|r| r.tx_power_dbm.is_none() && r.noise_dbm.is_none()));

        let other = &inv.aps[1];
        assert_eq!(other.mac.to_string(), "70:6d:15:0a:04:c0");
        assert_eq!(
            (other.run_state.as_str(), other.state),
            ("downloading", WlanApState::NotAssociated)
        );
        assert_eq!(
            other.clients, None,
            "no radio answered, so no count — never a 0"
        );
        assert!(other.radios.is_empty());
    }

    /// `bsnAPIfOperStatus` is down(1)/up(2) — the reverse of Huawei's up(1)/down(2), so reading
    /// Cisco rows with the Huawei rule would show every working radio down.
    #[test]
    fn a_cisco_radio_reads_up_as_two_and_down_as_one() {
        for (status, up) in [(2, Some(true)), (1, Some(false)), (3, None)] {
            let s0 = with_slot(SERVING, 0);
            let rows = vec![
                at(CISCO_RADIO, 2, &s0, SnmpValue::Int(1)),
                at(CISCO_RADIO, 12, &s0, SnmpValue::Int(status)),
            ];
            let by_ap = radios(WlanFlavor::CiscoAirespace, &rows);
            assert_eq!(
                by_ap[&ApMac::from_subids(&SERVING).unwrap()][0].up,
                up,
                "{status}"
            );
        }
        // A radio whose band neither its type nor its channel settles is not guessed.
        let s0 = with_slot(SERVING, 0);
        let undecided = vec![
            at(CISCO_RADIO, 2, &s0, SnmpValue::Int(7)),
            at(CISCO_RADIO, 4, &s0, SnmpValue::Int(37)),
        ];
        assert!(radios(WlanFlavor::CiscoAirespace, &undecided).is_empty());
    }

    #[test]
    fn a_cisco_ssid_is_named_by_its_column_and_one_ssid_on_two_wlans_is_one_ssid() {
        let rows = vec![
            at(CISCO_SSID, 2, &[1], bytes("corp")),
            at(CISCO_SSID, 38, &[1], SnmpValue::Int(3)),
            at(CISCO_SSID, 2, &[2], bytes("guest")),
            at(CISCO_SSID, 38, &[2], SnmpValue::Int(0)),
            at(CISCO_SSID, 2, &[3], bytes("corp")),
            at(CISCO_SSID, 38, &[3], SnmpValue::Int(4)),
            // A WLAN with no name is nobody's SSID — but its clients are the controller's (H2).
            at(CISCO_SSID, 38, &[4], SnmpValue::Int(9)),
        ];
        let table = ssid_table(WlanFlavor::CiscoAirespace, &rows);
        let readings = &table.readings;
        let by_name: Vec<(&str, Option<u32>, u32)> = readings
            .iter()
            .map(|r| (r.name.as_str(), r.clients(), r.row))
            .collect();
        let mut expected = vec![
            ("corp", Some(7), ssid_row_key("corp")),
            ("guest", Some(0), ssid_row_key("guest")),
        ];
        expected.sort_by_key(|(_, _, row)| *row);
        assert_eq!(by_name, expected);
        assert!(readings
            .iter()
            .all(|r| r.clients_2g4.is_none() && r.ap_count.is_none()));
        assert_eq!(
            table.clients_total,
            Some(16),
            "3 + 0 + 4, and 9 on the unnamed WLAN"
        );
        assert_eq!(table.unnamed_rows, 1);
        // An empty SSID still gets its series and its name.
        let (samples, names) = ssid_samples(readings);
        assert_eq!(samples.len(), 2);
        assert_eq!(names.len(), 2);
        assert_eq!(
            ssid_columns(WlanFlavor::CiscoAirespace),
            vec![
                format!("{CISCO_SSID}.2"),
                format!("{CISCO_SSID}.38"),
                CLW_SSID.to_owned(),
            ]
        );
    }

    /// The column `cLWlanSsid` lives at, per WLAN number (CISCO-LWAPP-WLAN-MIB).
    const CLW_SSID: &str = "1.3.6.1.4.1.9.9.512.1.1.1.1.4";

    fn clw(wlan: u32, name: &str) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: CLW_SSID.to_owned(),
            instance: vec![wlan],
            value: bytes(name),
        }
    }

    /// 🚨 The 9800 bug (ADR-064 増分 H, H1): the lab's recording answers `.38` for its eight WLANs
    /// and `.2` for none. Named from `cLWlanSsid`, it has its SSIDs; with `.2` alone it had none, an
    /// SSID count of 0 and no controller client count. Values are the recording's own.
    #[test]
    fn a_9800_that_does_not_answer_the_ssid_column_is_named_from_the_wlan_table() {
        let recording = [
            (1, "<private>", 5),
            (2, "NAI-Mobile", 7),
            (3, "NAI-IT", 0),
            (4, "NA Guest", 0),
            (5, "SAP", 7),
            (6, "TIME", 9),
            (7, "OTN", 12),
            (8, "NAI-TPE", 1),
        ];
        let mut rows = Vec::new();
        for (wlan, name, clients) in recording {
            rows.push(at(CISCO_SSID, 38, &[wlan], SnmpValue::Int(clients)));
            rows.push(clw(wlan, name));
        }
        let table = ssid_table(WlanFlavor::CiscoAirespace, &rows);
        assert_eq!(table.readings.len(), 8);
        assert_eq!(table.unnamed_rows, 0);
        assert_eq!(table.clients_total, Some(41));
        let otn = table
            .readings
            .iter()
            .find(|r| r.name == "OTN")
            .expect("named from cLWlanSsid");
        assert_eq!(otn.clients(), Some(12));
    }

    /// AireOS answers both names, identically: one SSID each, never two. Where they differ the
    /// SSID table's own column wins; a `cLWlanSsid` row for a WLAN the SSID table does not have is
    /// ignored, because the two tables' numbering agreeing is the assumption being relied on.
    #[test]
    fn the_ssid_column_outranks_the_wlan_table_and_a_stray_wlan_row_is_ignored() {
        let rows = vec![
            at(CISCO_SSID, 2, &[1], bytes("office24")),
            at(CISCO_SSID, 38, &[1], SnmpValue::Int(0)),
            clw(1, "office24"),
            at(CISCO_SSID, 2, &[2], bytes("office5")),
            at(CISCO_SSID, 38, &[2], SnmpValue::Int(2)),
            clw(2, "renamed-elsewhere"),
            clw(9, "not-in-the-ssid-table"),
        ];
        let table = ssid_table(WlanFlavor::CiscoAirespace, &rows);
        let mut names: Vec<&str> = table.readings.iter().map(|r| r.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["office24", "office5"]);
        assert_eq!(table.clients_total, Some(2));
        assert_eq!(table.unnamed_rows, 0);
    }

    /// A table with no client count anywhere says nothing about clients, rather than 0.
    #[test]
    fn a_cisco_ssid_table_with_no_client_column_has_no_total() {
        let table = ssid_table(WlanFlavor::CiscoAirespace, &[clw(1, "corp")]);
        assert!(table.readings.is_empty(), "no WLAN the SSID table names");
        assert_eq!(table.clients_total, None);
        let table = ssid_table(
            WlanFlavor::CiscoAirespace,
            &[at(CISCO_SSID, 2, &[1], bytes("corp"))],
        );
        assert_eq!(table.readings.len(), 1);
        assert_eq!(table.clients_total, None);
    }

    /// The controller's clients per band are its radios' clients, summed by the band each radio was
    /// filed under (H4): a type the MIB names decides, anything else falls to the channel, and a
    /// radio neither settles is left out — as it is from the AP's slots.
    #[test]
    fn the_controllers_clients_per_band_are_its_radios_summed_by_band() {
        let ap1 = [0, 60, 16, 104, 153, 160];
        let ap2 = [0, 60, 16, 104, 153, 176];
        let mut rows = Vec::new();
        for (mac, slot, kind, channel, clients) in [
            (ap1, 0, 1, 11, 2),  // dot11b ⇒ 2.4 GHz
            (ap1, 1, 2, 36, 5),  // dot11a ⇒ 5 GHz
            (ap2, 0, 3, 6, 4),   // unlisted type, channel 6 ⇒ 2.4 GHz
            (ap2, 1, 3, 132, 1), // unlisted type, channel 132 ⇒ 5 GHz
            (ap2, 2, 5, 0, 7),   // unlisted type on channel 0 — no band, left out
            (ap2, 3, 7, 36, 3),  // 5/6 GHz switching radio — refused, left out
        ] {
            let index = with_slot(mac, slot);
            rows.push(at(CISCO_RADIO, 2, &index, SnmpValue::Int(kind)));
            rows.push(at(CISCO_RADIO, 4, &index, SnmpValue::Int(channel)));
            rows.push(at(CISCO_RADIO, 15, &index, SnmpValue::Int(clients)));
        }
        let per_band = clients_per_band(WlanFlavor::CiscoAirespace, &rows).expect("answered");
        assert_eq!(
            per_band.into_iter().collect::<Vec<_>>(),
            vec![
                (WlanBand::Band2G4, 6),
                (WlanBand::Band5G, 6),
                (WlanBand::Band6G, 0),
            ],
            "every band present, 0 where nobody is"
        );
    }

    /// Radios read, not one client count answered: no reading, never "nobody is connected".
    #[test]
    fn radios_that_answered_no_client_count_give_no_per_band_total() {
        let index = with_slot([0, 60, 16, 104, 153, 160], 0);
        let rows = vec![
            at(CISCO_RADIO, 2, &index, SnmpValue::Int(1)),
            at(CISCO_RADIO, 4, &index, SnmpValue::Int(6)),
        ];
        assert_eq!(clients_per_band(WlanFlavor::CiscoAirespace, &rows), None);
        assert_eq!(clients_per_band(WlanFlavor::CiscoAirespace, &[]), None);
    }

    /// AireOS's object first, then the 9800's; each platform answers only one. A 0 is an object
    /// answering its default, not a controller that holds no APs, and a Huawei AC is never asked.
    #[test]
    fn the_ap_capacity_is_whichever_platform_object_answered() {
        use yagra_transport::SnmpSample;
        let s = |oid: &str, value: f64| SnmpSample {
            oid: oid.to_owned(),
            value,
        };
        let aireos = [
            s("1.3.6.1.2.1.1.3.0", 5.0),
            s("1.3.6.1.4.1.14179.1.1.1.18.0", 150.0),
        ];
        assert_eq!(ap_capacity(WlanFlavor::CiscoAirespace, &aireos), Some(150));
        let c9800 = [s(".1.3.6.1.4.1.9.9.513.1.3.28.0", 250.0)];
        assert_eq!(ap_capacity(WlanFlavor::CiscoAirespace, &c9800), Some(250));
        let zero = [
            s("1.3.6.1.4.1.14179.1.1.1.18.0", 0.0),
            s("1.3.6.1.4.1.9.9.513.1.3.28.0", 250.0),
        ];
        assert_eq!(
            ap_capacity(WlanFlavor::CiscoAirespace, &zero),
            Some(250),
            "a 0 is not a reading, so the next object speaks"
        );
        assert_eq!(
            ap_capacity(
                WlanFlavor::CiscoAirespace,
                &[s("1.3.6.1.4.1.14179.1.1.1.18.0", f64::NAN)]
            ),
            None
        );
        assert!(capacity_oids(WlanFlavor::Huawei).is_empty());
        assert_eq!(ap_capacity(WlanFlavor::Huawei, &aireos), None);
    }

    /// The controller's joined count is the associated rows of the required walk — counted before
    /// any cap, and only rows whose index is an AP (F8).
    #[test]
    fn the_joined_count_is_the_associated_rows() {
        let mut rows = cisco_ap_rows();
        rows.push(at(CISCO_AP, 6, &[1, 2, 3], SnmpValue::Int(1)));
        assert_eq!(joined_count(WlanFlavor::CiscoAirespace, &rows), 1);
        let inv = inventory(WlanFlavor::CiscoAirespace, &rows, &[], &[], 1);
        assert_eq!(inv.aps.len(), 1, "cut to the cap");
        assert_eq!(
            joined_count(WlanFlavor::CiscoAirespace, &rows),
            1,
            "the count is not"
        );
        assert_eq!(joined_count(WlanFlavor::CiscoAirespace, &[]), 0);
    }
}
