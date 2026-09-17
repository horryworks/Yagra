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
use yagra_common::{
    huawei_run_state, sanitize_wlan_text, ApMac, WlanApObservation, WlanFlavor, WlanInventory,
    MAX_APS_PER_CONTROLLER_HARD,
};
use yagra_transport::{SnmpInstanceRow, SnmpValue};

/// Huawei `hwWlanApEntry` columns read, as `(column number, field)`. Numbers measured on the PoC's
/// AC6508 and matching HUAWEI-WLAN-AP-MIB.
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
}

/// Huawei's `hwWlanApTemperature` for an AP with no sensor. Measured: 36 of the PoC's 38 APs.
const HUAWEI_NO_TEMPERATURE: i64 = 255;

/// The most rows one AP walk takes, across every column.
///
/// Twice the hard AP cap per column, so a controller slightly over its cap still yields a complete
/// walk — the inventory is then cut and says so. A controller with more APs than this reads as an
/// incomplete walk (`wlan_ap_walk_complete` = 0), never as a partial list.
#[must_use]
pub fn walk_row_budget(flavor: WlanFlavor) -> usize {
    let per_column = usize::try_from(MAX_APS_PER_CONTROLLER_HARD).unwrap_or(usize::MAX) * 2;
    columns(flavor).len() * per_column
}

/// The column OIDs to walk for a dialect, run state first.
#[must_use]
pub fn columns(flavor: WlanFlavor) -> Vec<String> {
    match flavor {
        WlanFlavor::Huawei => HUAWEI_COLUMNS
            .iter()
            .map(|(n, _)| format!("{}.{n}", flavor.root_oid()))
            .collect(),
    }
}

/// The inventory in a dialect's rows, bounded to `max_aps` and the byte budget
/// ([`WlanInventory::bounded`]).
#[must_use]
pub fn inventory(flavor: WlanFlavor, rows: &[SnmpInstanceRow], max_aps: u32) -> WlanInventory {
    match flavor {
        WlanFlavor::Huawei => {
            WlanInventory::bounded(flavor, huawei_observations(flavor, rows), max_aps)
        }
    }
}

fn huawei_observations(flavor: WlanFlavor, rows: &[SnmpInstanceRow]) -> Vec<WlanApObservation> {
    let field_of: BTreeMap<String, Field> = HUAWEI_COLUMNS
        .iter()
        .map(|(n, f)| (format!("{}.{n}", flavor.root_oid()), *f))
        .collect();
    // Keyed by MAC, the run state creating the entry. Other columns only fill an existing one, and
    // are collected first so their order in `rows` does not matter.
    let mut states: BTreeMap<ApMac, (String, yagra_common::WlanApState)> = BTreeMap::new();
    let mut rest: BTreeMap<ApMac, Vec<(Field, &SnmpValue)>> = BTreeMap::new();
    for row in rows {
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
                    Field::Temp => {
                        obs.temp_c = match value {
                            SnmpValue::Int(HUAWEI_NO_TEMPERATURE) => None,
                            SnmpValue::Int(v) => i32::try_from(*v).ok(),
                            SnmpValue::Bytes(_) | SnmpValue::Oid(_) => None,
                        }
                    }
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

fn non_negative(value: &SnmpValue) -> Option<u32> {
    match value {
        SnmpValue::Int(v) => u32::try_from(*v).ok(),
        SnmpValue::Bytes(_) | SnmpValue::Oid(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::WlanApState;

    const ROOT: &str = "1.3.6.1.4.1.2011.6.139.13.3.3.1";

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
        let inv = inventory(WlanFlavor::Huawei, &active_controller_rows(), 1024);
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
        let inv = inventory(WlanFlavor::Huawei, &active_controller_rows(), 1024);
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
        let inv = inventory(WlanFlavor::Huawei, &rows, 1024);
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
        assert!(inventory(WlanFlavor::Huawei, &rows, 1024).aps.is_empty());
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
        assert!(inventory(WlanFlavor::Huawei, &rows, 1024).aps.is_empty());
    }

    #[test]
    fn device_strings_are_cleaned_and_non_utf8_survives() {
        let mac = [1, 2, 3, 4, 5, 6];
        let rows = vec![
            row(6, mac, SnmpValue::Int(8)),
            row(4, mac, SnmpValue::Bytes(b"ap\x00\xff-1\n".to_vec())),
        ];
        let inv = inventory(WlanFlavor::Huawei, &rows, 1024);
        assert_eq!(inv.aps[0].name.as_deref(), Some("ap \u{fffd}-1"));
    }

    #[test]
    fn a_controller_over_its_cap_is_cut_and_says_so() {
        let rows: Vec<_> = (0u32..6)
            .map(|i| row(6, [0, 0, 0, 0, 0, i], SnmpValue::Int(8)))
            .collect();
        let inv = inventory(WlanFlavor::Huawei, &rows, 4);
        assert_eq!(inv.aps.len(), 4);
        assert_eq!(inv.truncated_at, Some(6));
    }
}
