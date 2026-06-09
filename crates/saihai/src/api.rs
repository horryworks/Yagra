//! Northbound REST API (`/api/v1`).
//!
//! Path-versioned (ADR-019). Responses are JSON; errors use the fixed envelope
//! `{"error": {"code", "message"}}` so clients never see a raw internal error. The
//! skeleton exposes the latest reading for a node metric, served from the
//! [`InMemorySink`]; cursor pagination, SSE streams, and RBAC scoping land as the API grows.

use crate::sink::InMemorySink;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;
use yagra_common::{NodeId, SeriesKey};

/// Build the `/api/v1` router backed by the given metric sink.
pub fn router(sink: Arc<InMemorySink>) -> Router {
    Router::new()
        .route(
            "/api/v1/nodes/:node_id/metrics/:metric",
            get(get_node_metric),
        )
        .with_state(sink)
}

/// Latest reading for one node metric.
#[derive(Serialize)]
struct MetricReading {
    node_id: NodeId,
    metric: String,
    value: f64,
}

/// The fixed error envelope (ADR-019).
#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: String,
    message: String,
}

fn not_found(code: &str, message: String) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: ErrorDetail {
                code: code.to_owned(),
                message,
            },
        }),
    )
        .into_response()
}

async fn get_node_metric(
    State(sink): State<Arc<InMemorySink>>,
    Path((node_id, metric)): Path<(Uuid, String)>,
) -> Response {
    let node = NodeId::from(node_id);
    let key = SeriesKey::node(node, metric.as_str());
    match sink.latest(&key) {
        Some(value) => Json(MetricReading {
            node_id: node,
            metric,
            value,
        })
        .into_response(),
        None => not_found(
            "metric_not_found",
            format!("no reading for metric '{metric}' on node {node_id}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::MetricSink;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use hikyaku::{CheckOutcome, PollResult, Sample};
    use tower::ServiceExt; // for `oneshot`

    fn sink_with_reading(node: NodeId, metric: &str, value: f64) -> Arc<InMemorySink> {
        let sink = Arc::new(InMemorySink::default());
        sink.ingest(&PollResult {
            schema_version: 1,
            job_id: Uuid::nil(),
            node_id: node,
            at_unix_ms: 0,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge(metric, value)],
        });
        sink
    }

    #[tokio::test]
    async fn returns_latest_reading() {
        let node = NodeId::from(Uuid::nil());
        let app = router(sink_with_reading(node, "icmp_rtt_ms", 8.0));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/metrics/icmp_rtt_ms"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["metric"], "icmp_rtt_ms");
        assert_eq!(json["value"], 8.0);
    }

    #[tokio::test]
    async fn missing_metric_returns_error_envelope() {
        let node = NodeId::from(Uuid::nil());
        let app = router(Arc::new(InMemorySink::default()));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/metrics/icmp_rtt_ms"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "metric_not_found");
    }
}
