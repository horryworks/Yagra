// SPDX-License-Identifier: AGPL-3.0-only
//! The values a vendor table column answers with when a row has **no reading at all** (ADR-156).
//!
//! Some agents do not omit a row whose entity has no sensor: they answer it with a fixed placeholder.
//! Stored as-is, a placeholder is indistinguishable from a measurement — measured on a Huawei AC6508
//! (V200R024C00SPC100) on 2026-09-17, `hwEntityTemperature` answered **58** for its one real sensor
//! (SRU Board 0), **2147483647** for the chassis, the board slot and all ten GE ports, and **0** for
//! the two XGE ports. The node read 2147483647 °C and raised twelve per-row critical alerts.
//!
//! ## Why the marker is declared per column OID
//!
//! What a placeholder looks like is a fact about the **MIB column**, not about the template that
//! happens to collect it — the same reasoning as [`crate::row_names`]. So the answer is one `const`
//! table keyed by the column OID, and an operator template that collects the same column inherits it.
//!
//! ## What is deliberately not here
//!
//! - **`0`.** Zero is a real temperature, and a row reading zero is already hidden from the screen
//!   (ADR-143 decision 9). The two XGE rows above are left as readings.
//! - **An unmeasured marker.** A column absent from the table has no *known* placeholder, which is
//!   not the same as having none — guessing one would drop real readings silently.
//! - **Optical readings.** The poller scales a transceiver's light level before it reaches the bus,
//!   so only the poller can see that dialect's raw placeholder: it lives on
//!   `yagra_poller::optical::SimpleDialect::no_module` (ADR-062), and an `Optical` item never has one
//!   here.

use crate::collection::{CollectionItem, CollectionKind};

/// Column base OID → the value that means "this row has no reading". One line per measured column,
/// with the device it was measured on as the reason.
///
/// ⚠️ **Every entry must be a gauge table column the built-in catalogue collects**
/// (`every_declared_column_is_a_vendor_gauge_table_the_catalogue_collects`).
const NO_READING_COLUMNS: &[(&str, f64)] = &[
    // HUAWEI-ENTITY-EXTENT-MIB `hwEntityTemperature`. Huawei AC6508 V200R024C00SPC100, PoC
    // L37004wac002, 2026-09-17: 12 of 15 entity rows (chassis, board slot, GE0/0/1-10), while the one
    // sensor read 58 and its `hwEntityTemperatureThreshold` read 85. i32::MAX, the same class of
    // placeholder H3C uses for an empty transceiver slot.
    ("1.3.6.1.4.1.2011.5.25.31.1.1.1.1.11", 2_147_483_647.0),
];

/// The value `item` answers with for a row that has no reading, if one has been measured.
///
/// Only a table column can have one: a scalar is one value with nothing beside it to be a
/// placeholder for, and an optical item is scaled on the poller before any value reaches core.
#[must_use]
pub fn no_reading_marker(item: &CollectionItem) -> Option<f64> {
    match item.kind {
        CollectionKind::Table => {
            let oid = item.oid.trim_start_matches('.');
            NO_READING_COLUMNS
                .iter()
                .find(|(column, _)| *column == oid)
                .map(|(_, marker)| *marker)
        }
        CollectionKind::Scalar | CollectionKind::Optical => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{builtin_catalog, builtin_templates, item_publishes_per_interface};
    use crate::metric::MetricKind;

    fn builtin_items() -> Vec<CollectionItem> {
        builtin_templates()
            .into_iter()
            .flat_map(|t| t.items)
            .chain(builtin_catalog())
            .collect()
    }

    fn item(oid: &str, kind: CollectionKind) -> CollectionItem {
        CollectionItem {
            metric_name: "huawei_temp".to_owned(),
            oid: oid.to_owned(),
            kind,
            metric_kind: MetricKind::Gauge,
        }
    }

    /// 🚨 A declared column that nothing collects is dead, and one the catalogue collects as a
    /// counter or per interface would drop values core reads on a different path.
    #[test]
    fn every_declared_column_is_a_vendor_gauge_table_the_catalogue_collects() {
        assert!(
            !NO_READING_COLUMNS.is_empty(),
            "the table is empty, so the check below looked at nothing"
        );
        let items = builtin_items();
        for (column, marker) in NO_READING_COLUMNS {
            let collected: Vec<&CollectionItem> = items
                .iter()
                .filter(|i| i.oid.trim_start_matches('.') == *column)
                .collect();
            assert!(
                !collected.is_empty(),
                "NO_READING_COLUMNS declares {column}, which no built-in item collects"
            );
            for i in collected {
                assert_eq!(
                    i.kind,
                    CollectionKind::Table,
                    "{} is not a table column",
                    i.metric_name
                );
                assert_eq!(
                    i.metric_kind,
                    MetricKind::Gauge,
                    "{} is not a gauge",
                    i.metric_name
                );
                assert!(
                    !item_publishes_per_interface(i),
                    "{} publishes per interface",
                    i.metric_name
                );
            }
            assert!(
                marker.is_finite() && *marker != 0.0,
                "{column}: a marker must be a finite non-zero value, got {marker}"
            );
        }
    }

    #[test]
    fn the_measured_huawei_temperature_marker_is_declared() {
        let temp = builtin_items()
            .into_iter()
            .find(|i| i.metric_name == "huawei_temp")
            .expect("the built-in catalogue collects huawei_temp");
        assert_eq!(no_reading_marker(&temp), Some(2_147_483_647.0));
    }

    #[test]
    fn only_a_measured_table_column_carries_a_marker() {
        let column = "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.11";
        assert_eq!(
            no_reading_marker(&item(column, CollectionKind::Table)),
            Some(2_147_483_647.0)
        );
        // A leading dot is the same OID.
        assert_eq!(
            no_reading_marker(&item(&format!(".{column}"), CollectionKind::Table)),
            Some(2_147_483_647.0)
        );
        assert_eq!(
            no_reading_marker(&item(column, CollectionKind::Scalar)),
            None
        );
        assert_eq!(
            no_reading_marker(&item(column, CollectionKind::Optical)),
            None
        );
        // The sibling columns of the same table (CPU `.5`, memory `.7`) have no measured marker.
        assert_eq!(
            no_reading_marker(&item(
                "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.5",
                CollectionKind::Table
            )),
            None
        );
        assert_eq!(
            no_reading_marker(&item(
                "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.7",
                CollectionKind::Table
            )),
            None
        );
        // A prefix of the column is not the column.
        assert_eq!(
            no_reading_marker(&item(
                "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.1",
                CollectionKind::Table
            )),
            None
        );
    }
}
