//! Northbound REST API (`/api/v1`).
//!
//! Path-versioned (ADR-019). Responses are JSON; errors use the fixed envelope
//! `{"error": {"code", "message"}}` so clients never see a raw internal error. Readings
//! come from the [`MetricStore`] (VictoriaMetrics live, in-memory for the skeleton) and
//! the inventory from a [`NodeListing`]. Cursor pagination and RBAC scoping land as the
//! API grows; the alert endpoints are stubs until the alert engine is wired (Workstream B).

use crate::repo::NodeListing;
use crate::store::{MetricPoint, MetricStore};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use yagra_common::{NodeId, NodeState, SeriesKey};

/// Default range window when `from`/`to` are omitted (seconds).
const DEFAULT_RANGE_SECS: i64 = 3600;
/// Default range step when `step` is omitted (seconds).
const DEFAULT_STEP_SECS: u64 = 60;

/// Shared API state: the metric store and the node inventory source.
#[derive(Clone)]
pub struct ApiState {
    /// TSDB read/write seam.
    pub store: Arc<dyn MetricStore>,
    /// Inventory read seam.
    pub nodes: Arc<dyn NodeListing>,
}

/// Build the `/api/v1` router backed by the given state.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/v1/nodes", get(list_nodes))
        .route(
            "/api/v1/nodes/:node_id/metrics/:metric",
            get(get_node_metric),
        )
        .route(
            "/api/v1/nodes/:node_id/metrics/:metric/range",
            get(get_node_metric_range),
        )
        .route("/api/v1/alerts", get(list_alerts))
        .route("/api/v1/stream/alerts", get(stream_alerts))
        .with_state(state)
}

/// Liveness probe for the deploy/orchestrator — no auth, no store access.
async fn healthz() -> &'static str {
    "ok"
}

fn now_unix_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

// ── Response shapes ──────────────────────────────────────────────────────────

/// Latest reading for one node metric.
#[derive(Serialize)]
struct MetricReading {
    node_id: NodeId,
    metric: String,
    value: f64,
}

/// A time-series window for one node metric.
#[derive(Serialize)]
struct MetricRange {
    node_id: NodeId,
    metric: String,
    points: Vec<MetricPoint>,
}

/// One inventory row (mirrors the WebUI `NodeSummary`).
#[derive(Serialize)]
struct NodeSummary {
    id: NodeId,
    name: String,
    address: String,
    state: NodeState,
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

// ── Handlers ─────────────────────────────────────────────────────────────────

async fn list_nodes(State(st): State<ApiState>) -> Response {
    match st.nodes.list().await {
        Ok(nodes) => {
            let mut out = Vec::with_capacity(nodes.len());
            for n in nodes {
                // Until the alert state machine is wired (Workstream B), derive a coarse
                // state from liveness: a recent RTT ⇒ ok, otherwise unknown.
                let live = st
                    .store
                    .latest(&SeriesKey::node(n.id, "icmp_rtt_ms"))
                    .await
                    .is_some();
                out.push(NodeSummary {
                    id: n.id,
                    name: n.name,
                    address: n.address.to_string(),
                    state: if live {
                        NodeState::Ok
                    } else {
                        NodeState::Unknown
                    },
                });
            }
            Json(out).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to list nodes");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "failed to list nodes".to_owned(),
            )
        }
    }
}

async fn get_node_metric(
    State(st): State<ApiState>,
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
    match st.store.latest(&key).await {
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

/// Query params for the range endpoint (all optional; sensible defaults applied).
#[derive(Deserialize)]
struct RangeQuery {
    from: Option<i64>,
    to: Option<i64>,
    step: Option<u64>,
}

async fn get_node_metric_range(
    State(st): State<ApiState>,
    Path((node_id, metric)): Path<(Uuid, String)>,
    Query(q): Query<RangeQuery>,
) -> Response {
    if !is_valid_metric_name(&metric) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_metric_name",
            format!("metric name {metric:?} is not a valid identifier"),
        );
    }
    let node = NodeId::from(node_id);
    let to = q.to.unwrap_or_else(now_unix_s);
    let from = q.from.unwrap_or(to - DEFAULT_RANGE_SECS);
    let step = q.step.unwrap_or(DEFAULT_STEP_SECS).max(1);
    let key = SeriesKey::node(node, metric.as_str());
    let points = st.store.range(&key, from, to, step).await;
    Json(MetricRange {
        node_id: node,
        metric,
        points,
    })
    .into_response()
}

/// Active alerts. Stub until the alert engine is wired (Workstream B) — always empty.
async fn list_alerts() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

/// Live alert stream (SSE, ADR-019). Stub until the alert engine is wired: holds the
/// connection open with keep-alive comments and emits no events yet (so the WebUI's
/// EventSource connects cleanly instead of reconnect-looping on a 404).
async fn stream_alerts() -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    Sse::new(futures::stream::pending::<Result<Event, Infallible>>())
        .keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::StaticNodeList;
    use crate::sink::InMemorySink;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt; // for `oneshot`
    use yagra_bus::{CheckOutcome, PollResult, Sample};

    fn state_with(store: Arc<dyn MetricStore>) -> ApiState {
        ApiState {
            store,
            nodes: Arc::new(StaticNodeList::demo()),
        }
    }

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

    async fn body_json(resp: Response) -> serde_json::Value {
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn returns_latest_reading() {
        let node = NodeId::from(Uuid::nil());
        let app = router(state_with(store_with_reading(node, "icmp_rtt_ms", 8.0)));

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
        let json = body_json(resp).await;
        assert_eq!(json["metric"], "icmp_rtt_ms");
        assert_eq!(json["value"], 8.0);
    }

    #[tokio::test]
    async fn missing_metric_returns_error_envelope() {
        let node = NodeId::from(Uuid::nil());
        let app = router(state_with(Arc::new(InMemorySink::default())));

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
        let json = body_json(resp).await;
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
        let app = router(state_with(Arc::new(InMemorySink::default())));
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
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "invalid_metric_name");
    }

    #[tokio::test]
    async fn lists_nodes_with_derived_state() {
        let node = NodeId::from(Uuid::nil());
        // Demo node has a live RTT ⇒ state "ok".
        let app = router(state_with(store_with_reading(node, "icmp_rtt_ms", 8.0)));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/nodes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json[0]["id"], node.to_string());
        assert_eq!(json[0]["state"], "ok");
    }

    #[tokio::test]
    async fn node_without_reading_is_unknown() {
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/nodes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let json = body_json(resp).await;
        assert_eq!(json[0]["state"], "unknown");
    }

    #[tokio::test]
    async fn range_returns_points_array() {
        let node = NodeId::from(Uuid::nil());
        // In-memory store has no history, so points is an empty array (not an error).
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/metrics/icmp_rtt_ms/range"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["metric"], "icmp_rtt_ms");
        assert!(json["points"].is_array());
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let app = router(state_with(Arc::new(InMemorySink::default())));
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
