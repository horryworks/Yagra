// SPDX-License-Identifier: AGPL-3.0-only
//! Traffic-flow analysis endpoints (ADR-031).
//!
//! Six aggregations over the flow store — trend series, top talkers, conversations, top ports,
//! protocols and top AS — each served twice: scoped to one exporter node
//! (`/nodes/:node_id/flow/*`) and fleet-wide across every exporter (`/flow/*`). The two differ
//! only in whether a node scope is supplied, so [`run_flow_agg`] is the single body and the
//! handlers exist purely because the routes need different extractors.
//!
//! Flow monitoring is opt-in: with no ClickHouse flow store configured, `ApiState::flows` is
//! `None` and every endpoint here answers `503 flow_unavailable` rather than pretending to have
//! no data.
//!
//! This is the first domain extracted from the old single-file `api.rs`, and the shape the rest
//! follow: handlers take their auth guard as an extractor ([`RequireView`]) and return
//! [`ApiResult`], and the module owns its own [`Router`], merged by [`super::router`].

use super::extract::{RequireView, Scoped, VisibleNode};
use super::{ApiError, ApiResult, ApiState, DEFAULT_RANGE_SECS};
use crate::flowstore::{
    FlowAsAgg, FlowConversation, FlowPoint, FlowPortAgg, FlowProtoAgg, FlowTalker,
};
use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    get_node_flow_series,
    get_node_flow_top_talkers,
    get_node_flow_conversations,
    get_node_flow_top_ports,
    get_node_flow_protocols,
    get_node_flow_top_as,
    get_flow_series,
    get_flow_top_talkers,
    get_flow_conversations,
    get_flow_top_ports,
    get_flow_protocols,
    get_flow_top_as
))]
pub(super) struct Doc;

/// The flow routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        // Per-node: one exporter's traffic.
        .route(
            "/api/v1/nodes/:node_id/flow/series",
            get(get_node_flow_series),
        )
        .route(
            "/api/v1/nodes/:node_id/flow/top-talkers",
            get(get_node_flow_top_talkers),
        )
        .route(
            "/api/v1/nodes/:node_id/flow/conversations",
            get(get_node_flow_conversations),
        )
        .route(
            "/api/v1/nodes/:node_id/flow/top-ports",
            get(get_node_flow_top_ports),
        )
        .route(
            "/api/v1/nodes/:node_id/flow/protocols",
            get(get_node_flow_protocols),
        )
        .route(
            "/api/v1/nodes/:node_id/flow/top-as",
            get(get_node_flow_top_as),
        )
        // Fleet-wide (all exporters) — the dashboard's Traffic-flow widgets.
        .route("/api/v1/flow/series", get(get_flow_series))
        .route("/api/v1/flow/top-talkers", get(get_flow_top_talkers))
        .route("/api/v1/flow/conversations", get(get_flow_conversations))
        .route("/api/v1/flow/top-ports", get(get_flow_top_ports))
        .route("/api/v1/flow/protocols", get(get_flow_protocols))
        .route("/api/v1/flow/top-as", get(get_flow_top_as))
}

/// Time-window + limit query for the flow endpoints. `from`/`to` are Unix seconds (defaulting to
/// the trailing hour); `limit` caps top-N rows. `proto`/`port`/`peer` are optional drill-down
/// filters and `dir` selects the AS side for the top-AS endpoint — all parsed into strong types
/// (unparseable values are ignored), so no untrusted string reaches the store query.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct FlowRangeQuery {
    from: Option<i64>,
    to: Option<i64>,
    limit: Option<u32>,
    proto: Option<String>,
    port: Option<String>,
    peer: Option<String>,
    asn: Option<String>,
    dir: Option<String>,
}

/// `503` when flow monitoring is disabled (no ClickHouse flow store configured).
fn flow_unavailable() -> ApiError {
    ApiError::unavailable(
        "flow_unavailable",
        "flow monitoring is not enabled (no ClickHouse flow store configured)",
    )
}

/// Refuse a group-scoped caller the **fleet-wide** aggregations (ADR-014).
///
/// ⚠️ The plan for this pass assumed these could be over-fetched and post-filtered like the TSDB
/// rankings. They cannot, and the reason is worth recording so nobody re-derives it: every fleet
/// aggregation here is a `GROUP BY` on something that is *not* the exporter — `GROUP BY dst_port`,
/// `GROUP BY addr`, `GROUP BY src, dst` — so ClickHouse collapses across exporters and **no
/// `node_id` survives into the row**. There is nothing left to filter on. (`FlowTalker`,
/// `FlowPortAgg`, `FlowProtoAgg`, `FlowAsAgg`, `FlowConversation` and `FlowPoint` all carry no node
/// field; that is the check.)
///
/// Pushing the restriction *down* instead — `node_id IN (…)` — would mean resolving the caller's
/// scope to a node-id list whose size is unbounded by anything, which is the full-fleet enumeration
/// `api/scope.rs` exists to avoid.
///
/// So a scoped caller is refused the roll-up and keeps `/nodes/{node_id}/flow/*`, which is
/// `NodeScoped` and is where flow analysis is actually done — you look at one exporter.
fn fleet_flow_is_unattributed(scope: &super::scope::NodeScope) -> Result<(), ApiError> {
    if scope.is_all() {
        return Ok(());
    }
    Err(ApiError::forbidden_code(
        "scope_unsupported",
        "fleet-wide flow aggregates are grouped by address, port, protocol or AS, so no exporter \
         attribution survives to filter on; query a specific node's flow instead",
    ))
}

/// Map a flow-store error to a `500` without leaking the internal error to the client
/// (security.md).
fn flow_query_failed(what: &str, e: &anyhow::Error) -> ApiError {
    tracing::warn!(error = %e, "flow {what} query failed");
    ApiError::internal("flow query failed")
}

/// Build a validated [`crate::flowstore::FlowQuery`] from a request. `node_id` is `Some` (from the
/// path) for a per-node query or `None` for the fleet-wide (all-exporters) endpoints; the window is
/// converted to ms and the limit clamped — no untrusted string ever reaches the store query.
fn flow_query_params(node_id: Option<Uuid>, q: &FlowRangeQuery) -> crate::flowstore::FlowQuery {
    let to = q.to.unwrap_or_else(super::now_unix_s);
    let from = q.from.unwrap_or(to - DEFAULT_RANGE_SECS);
    let limit = q.limit.unwrap_or(20).clamp(1, 1000);
    crate::flowstore::FlowQuery {
        node_id,
        from_unix_ms: from.saturating_mul(1000),
        to_unix_ms: to.saturating_mul(1000),
        limit,
        // Parse into strong types; anything unparseable is simply ignored (no raw string in SQL).
        proto: q.proto.as_deref().and_then(|s| s.trim().parse::<u8>().ok()),
        dst_port: q.port.as_deref().and_then(|s| s.trim().parse::<u16>().ok()),
        peer: q
            .peer
            .as_deref()
            .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok()),
        asn: q.asn.as_deref().and_then(|s| s.trim().parse::<u32>().ok()),
    }
}

/// Which AS side the top-AS endpoint aggregates on (`dir=src`; default destination).
fn flow_as_dir(q: &FlowRangeQuery) -> crate::flowstore::AsDir {
    match q.dir.as_deref() {
        Some("src") => crate::flowstore::AsDir::Src,
        _ => crate::flowstore::AsDir::Dst,
    }
}

/// Fill conversation `*_as_name` fields from the hot-swappable IP→ASN table (a reloader may swap it
/// concurrently, so snapshot once). Names are resolved here, never stored (flowstore.rs leaves them
/// `None`).
fn resolve_conversation_as_names(st: &ApiState, rows: &mut [crate::flowstore::FlowConversation]) {
    let db = st.ipasn.read().unwrap().clone();
    if let Some(db) = &db {
        for r in rows.iter_mut() {
            if r.src_asn != 0 {
                r.src_as_name = db.name_of(r.src_asn).map(str::to_owned);
            }
            if r.dst_asn != 0 {
                r.dst_as_name = db.name_of(r.dst_asn).map(str::to_owned);
            }
        }
    }
}

/// Fill AS-aggregate `name` fields from the hot-swappable IP→ASN table.
fn resolve_as_names(st: &ApiState, rows: &mut [crate::flowstore::FlowAsAgg]) {
    let db = st.ipasn.read().unwrap().clone();
    if let Some(db) = &db {
        for r in rows.iter_mut() {
            if r.asn != 0 {
                r.name = db.name_of(r.asn).map(str::to_owned);
            }
        }
    }
}

/// The six flow aggregations the API exposes. Adding a seventh is a variant here, an arm in
/// [`run_flow_agg`], and the two route lines in [`routes`].
#[derive(Clone, Copy)]
enum FlowAgg {
    Series,
    TopTalkers,
    Conversations,
    TopPorts,
    Protocols,
    TopAs,
}

impl FlowAgg {
    /// Log label for a failed query, e.g. `flow fleet top-as query failed`.
    fn label(self, fleet: bool) -> &'static str {
        match (self, fleet) {
            (Self::Series, false) => "series",
            (Self::Series, true) => "fleet series",
            (Self::TopTalkers, false) => "top-talkers",
            (Self::TopTalkers, true) => "fleet top-talkers",
            (Self::Conversations, false) => "conversations",
            (Self::Conversations, true) => "fleet conversations",
            (Self::TopPorts, false) => "top-ports",
            (Self::TopPorts, true) => "fleet top-ports",
            (Self::Protocols, false) => "protocols",
            (Self::Protocols, true) => "fleet protocols",
            (Self::TopAs, false) => "top-as",
            (Self::TopAs, true) => "fleet top-as",
        }
    }
}

/// Run one flow aggregation over an optional node scope (`None` ⇒ fleet-wide). This is the body
/// all twelve `/flow/*` endpoints share: the flow-disabled gate, query validation, the store call,
/// AS-name resolution, and error mapping. It used to be written out twelve times — six per-node
/// handlers and their fleet twins, each differing only in `Some(node_id)` vs `None` and a log
/// string — so a fix to any of that had to be applied twelve times to stay consistent.
async fn run_flow_agg(
    st: &ApiState,
    node_id: Option<Uuid>,
    q: &FlowRangeQuery,
    agg: FlowAgg,
) -> ApiResult<Response> {
    let store = st.flows.clone().ok_or_else(flow_unavailable)?;
    let fq = flow_query_params(node_id, q);
    let what = agg.label(node_id.is_none());
    Ok(match agg {
        FlowAgg::Series => {
            let sq = crate::flowstore::FlowSeriesQuery {
                node_id: fq.node_id,
                from_unix_ms: fq.from_unix_ms,
                to_unix_ms: fq.to_unix_ms,
                proto: fq.proto,
            };
            let points = store
                .series(&sq)
                .await
                .map_err(|e| flow_query_failed(what, &e))?;
            Json(points).into_response()
        }
        FlowAgg::TopTalkers => {
            let rows = store
                .top_talkers(&fq)
                .await
                .map_err(|e| flow_query_failed(what, &e))?;
            Json(rows).into_response()
        }
        FlowAgg::Conversations => {
            let mut rows = store
                .top_conversations(&fq)
                .await
                .map_err(|e| flow_query_failed(what, &e))?;
            resolve_conversation_as_names(st, &mut rows);
            Json(rows).into_response()
        }
        FlowAgg::TopPorts => {
            let rows = store
                .top_ports(&fq)
                .await
                .map_err(|e| flow_query_failed(what, &e))?;
            Json(rows).into_response()
        }
        FlowAgg::Protocols => {
            let rows = store
                .top_protocols(&fq)
                .await
                .map_err(|e| flow_query_failed(what, &e))?;
            Json(rows).into_response()
        }
        FlowAgg::TopAs => {
            let mut rows = store
                .top_as(&fq, flow_as_dir(q))
                .await
                .map_err(|e| flow_query_failed(what, &e))?;
            resolve_as_names(st, &mut rows);
            Json(rows).into_response()
        }
    })
}

/// Define the per-node and fleet handler pair for one aggregation. Both are thin: they exist only
/// because the two routes need different extractors (`Path<Uuid>` vs none). `RequireView` is an
/// argument, so neither can be wired up without its read guard.
///
/// The `#[utoipa::path]` lives here too (ADR-035). [`run_flow_agg`] erases to `Response` because the
/// six aggregations return six row types, but each *handler* has exactly one, so the row type is a
/// macro parameter and the document names the real shape rather than an opaque body. The two paths
/// and the row type are `tt`/`ident` fragments deliberately: any other fragment kind arrives wrapped
/// in invisible delimiters, which the attribute parser does not see through.
macro_rules! flow_endpoints {
    ($node_fn:ident, $fleet_fn:ident, $agg:expr, $node_path:tt, $fleet_path:tt, $row:ident, $desc:tt) => {
        #[doc = concat!("`GET ", $node_path, "` — for one exporter node.")]
        #[utoipa::path(
            get, path = $node_path, tag = "flow",
            params(("node_id" = Uuid, Path, description = "Exporter node id"), FlowRangeQuery),
            responses(
                (status = 200, description = $desc, body = Vec<$row>),
                (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
                (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
                (status = 503, description = "Flow monitoring is not enabled (no flow store configured)", body = super::error::ErrorBody),
            ),
        )]
        // One guard here covers all six per-node flow routes — the macro is why the pair was
        // deduplicated in the first place, and it pays off again: there is no sixth copy to forget.
        async fn $node_fn(
            _view: RequireView,
            _visible: VisibleNode,
            State(st): State<ApiState>,
            Path(node_id): Path<Uuid>,
            Query(q): Query<FlowRangeQuery>,
        ) -> ApiResult<Response> {
            run_flow_agg(&st, Some(node_id), &q, $agg).await
        }

        #[doc = concat!("`GET ", $fleet_path, "` — fleet-wide, across every exporter.")]
        #[utoipa::path(
            get, path = $fleet_path, tag = "flow",
            params(FlowRangeQuery),
            responses(
                (status = 200, description = $desc, body = Vec<$row>),
                (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
                (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
                (status = 503, description = "Flow monitoring is not enabled (no flow store configured)", body = super::error::ErrorBody),
            ),
        )]
        async fn $fleet_fn(
            _view: RequireView,
            Scoped(scope): Scoped,
            State(st): State<ApiState>,
            Query(q): Query<FlowRangeQuery>,
        ) -> ApiResult<Response> {
            fleet_flow_is_unattributed(&scope)?;
            run_flow_agg(&st, None, &q, $agg).await
        }
    };
}

flow_endpoints!(
    get_node_flow_series,
    get_flow_series,
    FlowAgg::Series,
    "/api/v1/nodes/{node_id}/flow/series",
    "/api/v1/flow/series",
    FlowPoint,
    "Traffic trend: bytes and packets per protocol per 5-minute bucket"
);
flow_endpoints!(
    get_node_flow_top_talkers,
    get_flow_top_talkers,
    FlowAgg::TopTalkers,
    "/api/v1/nodes/{node_id}/flow/top-talkers",
    "/api/v1/flow/top-talkers",
    FlowTalker,
    "Host addresses ranked by traffic"
);
flow_endpoints!(
    get_node_flow_conversations,
    get_flow_conversations,
    FlowAgg::Conversations,
    "/api/v1/nodes/{node_id}/flow/conversations",
    "/api/v1/flow/conversations",
    FlowConversation,
    "Source→destination pairs ranked by traffic, AS names resolved"
);
flow_endpoints!(
    get_node_flow_top_ports,
    get_flow_top_ports,
    FlowAgg::TopPorts,
    "/api/v1/nodes/{node_id}/flow/top-ports",
    "/api/v1/flow/top-ports",
    FlowPortAgg,
    "Destination ports ranked by traffic"
);
flow_endpoints!(
    get_node_flow_protocols,
    get_flow_protocols,
    FlowAgg::Protocols,
    "/api/v1/nodes/{node_id}/flow/protocols",
    "/api/v1/flow/protocols",
    FlowProtoAgg,
    "IP protocols ranked by traffic"
);
flow_endpoints!(
    get_node_flow_top_as,
    get_flow_top_as,
    FlowAgg::TopAs,
    "/api/v1/nodes/{node_id}/flow/top-as",
    "/api/v1/flow/top-as",
    FlowAsAgg,
    "Autonomous systems ranked by traffic, names resolved"
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    async fn get(st: ApiState, uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = router(st)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn every_endpoint_reports_flow_monitoring_as_disabled_when_unconfigured() {
        // Default-OFF is the shipped state, so this is the answer most deployments give. It must
        // be a typed 503 the UI can distinguish from "configured, but no traffic seen".
        for uri in [
            "/api/v1/flow/series",
            "/api/v1/flow/top-talkers",
            "/api/v1/flow/conversations",
            "/api/v1/flow/top-ports",
            "/api/v1/flow/protocols",
            "/api/v1/flow/top-as",
            "/api/v1/nodes/00000000-0000-0000-0000-000000000000/flow/series",
            "/api/v1/nodes/00000000-0000-0000-0000-000000000000/flow/top-as",
        ] {
            let (status, json) = get(public_state(), uri).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
            assert_eq!(json["error"]["code"], "flow_unavailable", "{uri}");
        }
    }

    #[tokio::test]
    async fn reads_are_gated_before_the_flow_store_is_consulted() {
        // The guard must run first: an anonymous caller on a private deployment learns it is
        // unauthenticated, never whether this install happens to have a flow store configured.
        let (status, json) = get(private_state(), "/api/v1/flow/top-talkers").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["error"]["code"], "unauthorized");
    }

    #[test]
    fn the_window_defaults_to_the_trailing_hour_and_clamps_the_limit() {
        let q = FlowRangeQuery {
            from: None,
            to: Some(1_000_000),
            limit: Some(99_999),
            proto: None,
            port: None,
            peer: None,
            asn: None,
            dir: None,
        };
        let fq = flow_query_params(None, &q);
        assert_eq!(fq.to_unix_ms, 1_000_000_000);
        assert_eq!(fq.from_unix_ms, (1_000_000 - DEFAULT_RANGE_SECS) * 1000);
        assert_eq!(fq.limit, 1000, "an unbounded top-N would be a DoS vector");
        assert_eq!(fq.node_id, None);
    }

    #[test]
    fn unparseable_filters_are_dropped_rather_than_reaching_the_store() {
        // security.md: strong types at the edge. A junk value must not become a SQL predicate.
        let q = FlowRangeQuery {
            from: None,
            to: None,
            limit: None,
            proto: Some("'; DROP TABLE flows--".to_owned()),
            port: Some("not-a-port".to_owned()),
            peer: Some("not-an-ip".to_owned()),
            asn: Some("AS15169".to_owned()),
            dir: None,
        };
        let fq = flow_query_params(None, &q);
        assert_eq!(fq.proto, None);
        assert_eq!(fq.dst_port, None);
        assert_eq!(fq.peer, None);
        assert_eq!(fq.asn, None);
    }

    #[test]
    fn the_as_direction_defaults_to_destination() {
        let with_dir = |dir: Option<&str>| FlowRangeQuery {
            from: None,
            to: None,
            limit: None,
            proto: None,
            port: None,
            peer: None,
            asn: None,
            dir: dir.map(str::to_owned),
        };
        assert!(matches!(
            flow_as_dir(&with_dir(Some("src"))),
            crate::flowstore::AsDir::Src
        ));
        assert!(matches!(
            flow_as_dir(&with_dir(Some("bogus"))),
            crate::flowstore::AsDir::Dst
        ));
        assert!(matches!(
            flow_as_dir(&with_dir(None)),
            crate::flowstore::AsDir::Dst
        ));
    }

    #[test]
    fn the_fleet_label_distinguishes_the_two_scopes_in_logs() {
        assert_eq!(FlowAgg::TopAs.label(false), "top-as");
        assert_eq!(FlowAgg::TopAs.label(true), "fleet top-as");
    }
}
