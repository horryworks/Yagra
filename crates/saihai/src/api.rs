//! Northbound REST API (`/api/v1`).
//!
//! Path-versioned (ADR-019). Responses are JSON; errors use the fixed envelope
//! `{"error": {"code", "message"}}` so clients never see a raw internal error. The
//! latest reading for a node metric is served from the [`MetricStore`] (VictoriaMetrics
//! live, in-memory for the skeleton); cursor pagination, SSE streams, and RBAC scoping
//! land as the API grows.

use crate::store::MetricStore;
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

/// Build the `/api/v1` router backed by the given metric store.
pub fn router(store: Arc<dyn MetricStore>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route(
            "/api/v1/nodes/:node_id/metrics/:metric",
            get(get_node_metric),
        )
        .with_state(store)
}

/// Liveness probe for the deploy/orchestrator — no auth, no store access.
async fn healthz() -> &'static str {
    "ok"
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

fn error_response(status: StatusCode, code: &str, message: String) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorDetail {
                code: code.to_owned(),
                message,
            },
        }),
    )
        .into_response()
}

fn not_found(code: &str, message: String) -> Response {
    error_response(StatusCode::NOT_FOUND, code, message)
}

/// A Prometheus-style metric name: `[a-zA-Z_:][a-zA-Z0-9_:]*`. Validating at the edge
/// keeps the (untrusted) path segment from being interpolated into the PromQL selector
/// sent to the TSDB (security.md: parse into strong, bounded types at the API edge).
fn is_valid_metric_name(metric: &str) -> bool {
    let mut chars = metric.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

async fn get_node_metric(
    State(store): State<Arc<dyn MetricStore>>,
    Path((node_id, metric)): Path<(Uuid, String)>,
) -> Response {
    if !is_valid_metric_name(&metric) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_metric_name",
            format!("metric name {metric:?} is not a valid identifier"),
        );
    }
    let node = NodeId::from(node_id);
    let key = SeriesKey::node(node, metric.as_str());
    match store.latest(&key).await {
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
    use crate::sink::InMemorySink;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use hikyaku::{CheckOutcome, PollResult, Sample};
    use tower::ServiceExt; // for `oneshot`

    fn store_with_reading(node: NodeId, metric: &str, value: f64) -> Arc<dyn MetricStore> {
        let sink = InMemorySink::default();
        sink.ingest(&PollResult {
            schema_version: 1,
            job_id: Uuid::nil(),
            node_id: node,
            at_unix_ms: 0,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge(metric, value)],
        });
        Arc::new(sink)
    }

    #[tokio::test]
    async fn returns_latest_reading() {
        let node = NodeId::from(Uuid::nil());
        let app = router(store_with_reading(node, "icmp_rtt_ms", 8.0));

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

    #[test]
    fn metric_name_validation_rejects_injection() {
        assert!(is_valid_metric_name("icmp_rtt_ms"));
        assert!(is_valid_metric_name("_internal:ratio"));
        // PromQL-injection attempts and stray characters are rejected.
        assert!(!is_valid_metric_name("up} or vector(1) #"));
        assert!(!is_valid_metric_name("a b"));
        assert!(!is_valid_metric_name("9starts_with_digit"));
        assert!(!is_valid_metric_name(""));
    }

    #[tokio::test]
    async fn invalid_metric_name_returns_bad_request() {
        let node = NodeId::from(Uuid::nil());
        let app = router(Arc::new(InMemorySink::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/metrics/bad%20name"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "invalid_metric_name");
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let app = router(Arc::new(InMemorySink::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
