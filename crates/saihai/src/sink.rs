//! Metric sink: where poll results land.
//!
//! [`MetricSink`] is the seam behind which the production TSDB (VictoriaMetrics) writer
//! sits. The skeleton uses [`InMemorySink`], which keeps only the latest value per
//! series so the API can serve it. Samples are keyed by their thin-label
//! [`SeriesKey`](yagra_common::SeriesKey) (ADR-011); raw counters are stored as-is and
//! rates are derived at query time (ADR-012).

use hikyaku::PollResult;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use yagra_common::SeriesKey;

/// Somewhere poll results can be written. The real implementation writes to the TSDB.
pub trait MetricSink: Send + Sync {
    /// Ingest all samples from a completed poll.
    fn ingest(&self, result: &PollResult);
}

/// An in-memory sink holding the latest value per series — enough for the skeleton API.
#[derive(Default)]
pub struct InMemorySink {
    latest: Mutex<HashMap<SeriesKey, f64>>,
}

impl InMemorySink {
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

impl MetricSink for InMemorySink {
    fn ingest(&self, result: &PollResult) {
        let mut map = self.latest.lock().expect("sink mutex poisoned");
        for sample in &result.samples {
            map.insert(sample.series_key(result.node_id), sample.value);
        }
    }
}

/// Consume poll results off the bus and write them to the sink. Returns when the result
/// channel closes.
pub async fn run_sink<S: MetricSink>(mut results: broadcast::Receiver<PollResult>, sink: Arc<S>) {
    loop {
        match results.recv().await {
            Ok(result) => sink.ingest(&result),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "sink lagged behind; some results were dropped");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
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
