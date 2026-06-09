//! In-memory metric sink for the skeleton / tests.
//!
//! Holds only the latest value per series so the API can serve it without a TSDB. The
//! live path uses [`crate::store::VmStore`] (VictoriaMetrics); both sit behind the
//! [`crate::store::MetricStore`] trait. Samples are keyed by their thin-label
//! [`SeriesKey`](yagra_common::SeriesKey) (ADR-011); raw counters are stored as-is and
//! rates are derived at query time (ADR-012).

use hikyaku::PollResult;
use std::collections::HashMap;
use std::sync::Mutex;
use yagra_common::SeriesKey;

/// An in-memory sink holding the latest value per series — enough for the skeleton API.
#[derive(Default)]
pub struct InMemorySink {
    latest: Mutex<HashMap<SeriesKey, f64>>,
}

impl InMemorySink {
    /// Ingest all samples from a completed poll, keeping the latest value per series.
    pub fn ingest(&self, result: &PollResult) {
        let mut map = self.latest.lock().expect("sink mutex poisoned");
        for sample in &result.samples {
            map.insert(sample.series_key(result.node_id), sample.value);
        }
    }

    /// The latest value recorded for a series, if any.
    #[must_use]
    pub fn latest(&self, key: &SeriesKey) -> Option<f64> {
        self.latest
            .lock()
            .expect("sink mutex poisoned")
            .get(key)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hikyaku::{CheckOutcome, Sample};
    use uuid::Uuid;
    use yagra_common::NodeId;

    fn result_with(node: NodeId, metric: &str, value: f64) -> PollResult {
        PollResult {
            schema_version: 1,
            job_id: Uuid::nil(),
            node_id: node,
            at_unix_ms: 0,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge(metric, value)],
        }
    }

    #[test]
    fn ingest_then_latest_returns_value() {
        let node = NodeId::new();
        let sink = InMemorySink::default();
        sink.ingest(&result_with(node, "icmp_rtt_ms", 9.0));

        assert_eq!(
            sink.latest(&SeriesKey::node(node, "icmp_rtt_ms")),
            Some(9.0)
        );
        assert_eq!(sink.latest(&SeriesKey::node(node, "missing")), None);
    }

    #[test]
    fn latest_value_overwrites_previous() {
        let node = NodeId::new();
        let sink = InMemorySink::default();
        sink.ingest(&result_with(node, "icmp_rtt_ms", 9.0));
        sink.ingest(&result_with(node, "icmp_rtt_ms", 4.0));
        assert_eq!(
            sink.latest(&SeriesKey::node(node, "icmp_rtt_ms")),
            Some(4.0)
        );
    }
}
