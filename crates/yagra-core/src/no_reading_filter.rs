// SPDX-License-Identifier: AGPL-3.0-only
//! Taking a vendor's "no reading" placeholder out of a poll result before anything stores or judges
//! it (ADR-156).
//!
//! The placeholder is declared per column OID in [`yagra_common::no_reading`]. Core never sees an
//! OID — a sample carries only its metric name — so this module turns the declaration into a
//! **metric name → marker** table from the same collection items the per-interface names come from
//! ([`crate::collection::per_interface_metric_names`]), and applies it once per result at the
//! ingest boundary.
//!
//! ## The shape worth knowing before editing
//!
//! - **One filter point, enforced by a type.** [`Admitted`] can only be built by
//!   [`NoReadingHandle::admit`], and both ingest paths persist and judge an `Admitted`, never a raw
//!   `PollResult`. A third entrance cannot store a placeholder without the compiler asking.
//! - **The stripped samples are not thrown away.** The live path hands them to the alert engine as
//!   evidence that a row has no reading, which is the only thing allowed to close that row's alert
//!   (ADR-156 決定 3–5). A row that simply stops arriving is not evidence and closes nothing.
//! - **An empty table is today's behaviour.** Before the first successful config load nothing is
//!   stripped: a placeholder is stored as a value, exactly as it was before this module existed, and
//!   nothing can be closed because closing needs an entry. A failed reload keeps the previous table
//!   (ADR-080) — the caller publishes only a table built from reads that all succeeded.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use yagra_bus::{PollResult, Sample};
use yagra_common::no_reading::no_reading_marker;
use yagra_common::CollectionItem;

/// Metric name → the value that means "this row has no reading".
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct NoReadingMarkers {
    by_metric: HashMap<String, f64>,
    /// Every distinct marker, so a sample whose value is not one of them is passed without hashing
    /// its name — the common case by far, on the hottest path core has.
    values: Vec<f64>,
}

impl NoReadingMarkers {
    /// The table for the collection items this deployment knows.
    ///
    /// ⚠️ **A name gets a marker only when every item publishing it agrees.** Two items may share a
    /// metric name with different OIDs — an operator can override `huawei_temp` at node scope with a
    /// column of their own — and core cannot tell their samples apart. Applying the marker to the name
    /// would then drop a reading from a column nobody measured. So a disagreement falls back to
    /// today's behaviour for that name, and says so in the log.
    pub(crate) fn from_items(items: &[CollectionItem]) -> Self {
        // name → the marker every item so far agrees on (`None` once any item disagrees)
        let mut agreed: HashMap<&str, Option<f64>> = HashMap::new();
        let mut declared: Vec<&str> = Vec::new();
        for item in items {
            let marker = no_reading_marker(item);
            if marker.is_some() && !declared.contains(&item.metric_name.as_str()) {
                declared.push(item.metric_name.as_str());
            }
            agreed
                .entry(item.metric_name.as_str())
                .and_modify(|slot| {
                    if *slot != marker {
                        *slot = None;
                    }
                })
                .or_insert(marker);
        }
        let mut by_metric = HashMap::new();
        let mut values: Vec<f64> = Vec::new();
        for name in declared {
            match agreed.get(name).copied().flatten() {
                Some(marker) => {
                    by_metric.insert(name.to_owned(), marker);
                    if !values.contains(&marker) {
                        values.push(marker);
                    }
                }
                None => tracing::info!(
                    metric = name,
                    "a collection item on a column with a known no-reading marker shares its metric \
                     name with an item that has a different one; that name's placeholder values are \
                     stored as readings (ADR-156)"
                ),
            }
        }
        Self { by_metric, values }
    }

    fn is_marker(&self, sample: &Sample) -> bool {
        self.values.contains(&sample.value)
            && self.by_metric.get(sample.metric.as_str()) == Some(&sample.value)
    }

    fn is_empty(&self) -> bool {
        self.by_metric.is_empty()
    }
}

/// The marker table currently in force, shared by the config refresh (which publishes it) and both
/// result consumers (which read it once per result).
///
/// Cheap to clone and to read, the same shape as [`crate::poll_interval::PollIntervals`].
#[derive(Debug, Clone, Default)]
pub(crate) struct NoReadingHandle(Arc<RwLock<Arc<NoReadingMarkers>>>);

impl NoReadingHandle {
    /// Replace the table. Called only with a table built from reads that all succeeded (ADR-080).
    pub(crate) fn publish(&self, markers: NoReadingMarkers) {
        // A poisoned lock can only come from a panic while holding it, and nothing panics under it.
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(markers);
    }

    /// Split `result` into what may be stored and judged, and the placeholder samples it carried.
    ///
    /// Allocates nothing for a result without a placeholder — every result, on a deployment whose
    /// devices send none.
    pub(crate) fn admit(&self, mut result: PollResult) -> Admitted {
        let markers = self
            .0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut no_reading = Vec::new();
        if !markers.is_empty() && result.samples.iter().any(|s| markers.is_marker(s)) {
            result.samples.retain(|s| {
                if markers.is_marker(s) {
                    no_reading.push(s.clone());
                    false
                } else {
                    true
                }
            });
        }
        if !no_reading.is_empty() {
            metrics::counter!("yagra_result_no_reading_samples_total")
                .increment(no_reading.len() as u64);
        }
        Admitted {
            result: Arc::new(result),
            no_reading,
        }
    }
}

/// A poll result whose placeholder samples have been taken out (ADR-156).
///
/// Only [`NoReadingHandle::admit`] builds one, which is what makes "filtered exactly once, before
/// anything reads the samples" a property of the types rather than of the call sites.
#[derive(Debug)]
pub(crate) struct Admitted {
    result: Arc<PollResult>,
    no_reading: Vec<Sample>,
}

impl Admitted {
    /// The result with its placeholder samples removed.
    pub(crate) fn result(&self) -> &Arc<PollResult> {
        &self.result
    }

    /// The placeholder samples, in the order the result carried them.
    pub(crate) fn no_reading(&self) -> &[Sample] {
        &self.no_reading
    }
}

/// The fifteen `hwEntityTemperature` rows the PoC AC6508 (S90001wac002) answered on 2026-09-17, in
/// walk order: twelve placeholders, the one sensor at 58, and two XGE ports at 0. Shared by every test
/// of ADR-156 so none of them re-types the measurement.
#[cfg(test)]
pub(crate) fn ac6508_temperature_rows() -> Vec<Sample> {
    let row = |r: u32, value: f64| {
        Sample::interface(
            "huawei_temp",
            yagra_common::IfIndex(r),
            value,
            yagra_common::MetricKind::Gauge,
        )
    };
    let mut rows = vec![row(3, AC6508_MARKER), row(5, AC6508_MARKER), row(9, 58.0)];
    for r in [14, 78, 142, 206, 270, 334, 398, 462, 526, 590] {
        rows.push(row(r, AC6508_MARKER));
    }
    rows.push(row(654, 0.0));
    rows.push(row(718, 0.0));
    rows
}

/// The placeholder in [`ac6508_temperature_rows`].
#[cfg(test)]
pub(crate) const AC6508_MARKER: f64 = 2_147_483_647.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::testkit::result;
    use yagra_bus::CheckOutcome;
    use yagra_common::{CollectionKind, IfIndex, MetricKind, NodeId};

    const TEMP_OID: &str = "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.11";
    const MARKER: f64 = AC6508_MARKER;

    fn item(name: &str, oid: &str, kind: CollectionKind) -> CollectionItem {
        CollectionItem {
            metric_name: name.to_owned(),
            oid: oid.to_owned(),
            kind,
            metric_kind: MetricKind::Gauge,
        }
    }

    fn huawei() -> NoReadingMarkers {
        NoReadingMarkers::from_items(&[
            item(
                "huawei_cpu_usage",
                "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.5",
                CollectionKind::Table,
            ),
            item("huawei_temp", TEMP_OID, CollectionKind::Table),
        ])
    }

    fn row(metric: &str, row: u32, value: f64) -> Sample {
        Sample::interface(metric, IfIndex(row), value, MetricKind::Gauge)
    }

    fn published(markers: NoReadingMarkers) -> NoReadingHandle {
        let handle = NoReadingHandle::default();
        handle.publish(markers);
        handle
    }

    #[test]
    fn the_ac6508_rows_lose_their_marker_and_keep_58_and_0() {
        let mut r = result(NodeId::new(), CheckOutcome::Reachable, 1);
        r.samples = ac6508_temperature_rows();
        let admitted = published(huawei()).admit(r);

        let kept: Vec<(u32, f64)> = admitted
            .result()
            .samples
            .iter()
            .map(|s| (s.ifindex.expect("a row").0, s.value))
            .collect();
        assert_eq!(kept, vec![(9, 58.0), (654, 0.0), (718, 0.0)]);
        let stripped: Vec<u32> = admitted
            .no_reading()
            .iter()
            .map(|s| s.ifindex.expect("a row").0)
            .collect();
        assert_eq!(
            stripped,
            vec![3, 5, 14, 78, 142, 206, 270, 334, 398, 462, 526, 590]
        );
        assert!(admitted.no_reading().iter().all(|s| s.value == MARKER));
    }

    /// The marker belongs to one column: the same value on a sibling column is a reading, and a result
    /// with no placeholder comes back as it went in.
    #[test]
    fn only_the_declared_metric_loses_the_value() {
        let mut r = result(NodeId::new(), CheckOutcome::Reachable, 1);
        r.samples = vec![
            row("huawei_cpu_usage", 3, MARKER),
            row("huawei_temp", 9, 58.0),
            Sample::gauge("icmp_rtt_ms", MARKER),
        ];
        let before = r.samples.clone();
        let admitted = published(huawei()).admit(r);
        assert_eq!(admitted.result().samples, before);
        assert!(admitted.no_reading().is_empty());
    }

    #[test]
    fn an_unpublished_handle_is_todays_behaviour() {
        let mut r = result(NodeId::new(), CheckOutcome::Reachable, 1);
        r.samples = ac6508_temperature_rows();
        let admitted = NoReadingHandle::default().admit(r);
        assert_eq!(admitted.result().samples.len(), 15);
        assert!(admitted.no_reading().is_empty());
    }

    /// A republished table replaces the previous one — which is how a config reload that removes the
    /// last collector of a column stops stripping its name.
    #[test]
    fn publishing_replaces_the_table() {
        let handle = published(huawei());
        handle.publish(NoReadingMarkers::default());
        let mut r = result(NodeId::new(), CheckOutcome::Reachable, 1);
        r.samples = vec![row("huawei_temp", 3, MARKER)];
        assert!(handle.admit(r).no_reading().is_empty());
    }

    #[test]
    fn a_name_shared_with_a_column_that_has_no_marker_gets_none() {
        // The built-in column twice agrees with itself.
        let twice = NoReadingMarkers::from_items(&[
            item("huawei_temp", TEMP_OID, CollectionKind::Table),
            item(
                "huawei_temp",
                &format!(".{TEMP_OID}"),
                CollectionKind::Table,
            ),
        ]);
        assert_eq!(twice.by_metric.get("huawei_temp"), Some(&MARKER));

        // An operator's override of the name on another column, and a scalar of the same name, each
        // veto it: core cannot tell those samples from the declared column's.
        for other in [
            item(
                "huawei_temp",
                "1.3.6.1.4.1.99999.1.1",
                CollectionKind::Table,
            ),
            item("huawei_temp", TEMP_OID, CollectionKind::Scalar),
        ] {
            let vetoed = NoReadingMarkers::from_items(&[
                item("huawei_temp", TEMP_OID, CollectionKind::Table),
                other.clone(),
            ]);
            assert!(vetoed.is_empty(), "{other:?} should have vetoed the marker");
            // …in either order.
            let vetoed = NoReadingMarkers::from_items(&[
                other,
                item("huawei_temp", TEMP_OID, CollectionKind::Table),
            ]);
            assert!(vetoed.is_empty());
        }
    }
}
