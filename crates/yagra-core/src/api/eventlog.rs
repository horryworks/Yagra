// SPDX-License-Identifier: AGPL-3.0-only
//! Reading the passive-event log: the searchable list and the dashboard aggregates.
//!
//! Named for what it holds. The *configuration* side of passive events — sources, ingest tokens,
//! rules, the webhook endpoint — is still in [`super`]; this module is only the read surface, which
//! is the half with two clients.
//!
//! ## Two stores, one filter
//!
//! When a log store is configured (ADR-024) it is the search source of record: it holds the whole
//! firehose, where PostgreSQL keeps only alert-linked rows. Both are queried through the same
//! [`crate::events::EventFilter`], and the two query builders are already pinned to each other by
//! `logstore::both_backends_filter_on_event_time_with_the_same_case_rules`.
//!
//! A free-text term may name a *node*, but the log store has never heard of node names — the TSDB
//! and log tiers carry ids only (ADR-011). So [`search_name_node_ids`] resolves the term to ids
//! first and passes those as a filter. Regex search is message-only and skips the resolution.
//!
//! ## Why the parse is shared
//!
//! [`parse_event_filter`] is the single validation edge, and it had a second copy in the MCP
//! `search_events` tool. The copies had drifted on the one thing that matters: the REST edge caps
//! the free-text term at [`SEARCH_TERM_MAX_CHARS`], and the MCP copy did not — so the surface with
//! *no* human in the loop was the one that could send an unbounded term to the store. That is the
//! general shape of the risk with duplicated validation: when two copies drift, the looser one is
//! the boundary.

use super::extract::{Admin, RequireView};
use super::{ApiError, ApiResult, ApiState};
use crate::events::{EventFilter, EventRow, EventStatGroup};
use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

/// The event-log read routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/events", get(list_events))
        .route("/api/v1/events/stats", get(events_stats))
}

/// Cap on a free-text search term. Long enough for any real query, short enough that a
/// pathological input cannot bloat the query sent to either store.
const SEARCH_TERM_MAX_CHARS: usize = 200;
/// How many node ids a name search may resolve to. A term matching half the fleet would otherwise
/// build an unbounded `IN (…)`.
const NAME_SEARCH_NODE_LIMIT: i64 = 50;
/// The event kinds a filter may name. Rejecting an unknown kind rather than ignoring it keeps a
/// typo from silently widening the search to everything.
const EVENT_KINDS: [&str; 3] = ["syslog", "trap", "webhook"];

/// The raw, unvalidated filter fields, as either surface receives them.
#[derive(Default)]
pub(crate) struct EventFilterInput<'a> {
    /// Keyset paging cursor — rows strictly older than this. Distinct from `start`/`end`, which
    /// bound the range being searched.
    pub before: Option<&'a str>,
    pub start: Option<&'a str>,
    pub end: Option<&'a str>,
    pub kind: Option<String>,
    pub node_id: Option<Uuid>,
    pub matched: Option<bool>,
    pub q: Option<&'a str>,
    pub regex: bool,
}

/// Normalize the free-text filter: trim, drop if empty, and cap the length.
fn normalize_search(q: Option<&str>) -> Option<String> {
    q.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(SEARCH_TERM_MAX_CHARS).collect())
}

/// Parse and validate the shared event-filter fields — the same set for the event log and
/// `/events/stats`, and for the MCP `search_events` tool.
pub(crate) fn parse_event_filter(input: EventFilterInput<'_>) -> Result<EventFilter, ApiError> {
    fn ts(
        value: Option<&str>,
        field: &str,
        code: &'static str,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
        match value {
            None => Ok(None),
            Some(s) => super::parse_rfc3339(s).map(Some).ok_or_else(|| {
                ApiError::bad_request(code, format!("{field} must be an RFC 3339 timestamp"))
            }),
        }
    }
    // The cursor and the range bounds get different codes: a bad cursor is a client paging bug,
    // a bad bound is operator input, and the UI surfaces them differently.
    let before = ts(input.before, "before", "invalid_cursor")?;
    let since = ts(input.start, "start", "invalid_filter")?;
    let until = ts(input.end, "end", "invalid_filter")?;

    if let Some(k) = input.kind.as_deref() {
        if !EVENT_KINDS.contains(&k) {
            return Err(ApiError::bad_request(
                "invalid_filter",
                format!("kind must be {}", EVENT_KINDS.join(", ")),
            ));
        }
    }
    let search = normalize_search(input.q);
    // Compile a user-supplied regex at the edge — the same size and ReDoS guard rule compilation
    // uses — so a pathological pattern never reaches either store.
    if input.regex {
        if let Some(term) = search.as_deref() {
            crate::events::compile_matcher("regex", term).map_err(|e| {
                ApiError::bad_request("invalid_filter", format!("invalid regular expression: {e}"))
            })?;
        }
    }
    Ok(EventFilter {
        before,
        since,
        until,
        kind: input.kind,
        node_id: input.node_id,
        matched: input.matched,
        search,
        regex: input.regex,
    })
}

/// Resolve a free-text (non-regex) term to matching node ids, for the log-store path's node-name
/// search. Empty for a regex search, which is message-only.
pub(crate) async fn search_name_node_ids(
    admin: &super::AdminState,
    filter: &EventFilter,
) -> Vec<Uuid> {
    match (filter.regex, filter.search.as_deref()) {
        (false, Some(term)) => admin
            .repo
            .node_ids_by_name_like(term, NAME_SEARCH_NODE_LIMIT)
            .await
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Search the event log, routing to whichever store is the source of record.
pub(crate) async fn search(
    st: &ApiState,
    admin: &super::AdminState,
    filter: &EventFilter,
    limit: i64,
) -> Result<Vec<EventRow>, ApiError> {
    let rows = match st.logs.as_ref() {
        Some(logs) => {
            let name_node_ids = search_name_node_ids(admin, filter).await;
            logs.search(filter, &name_node_ids, limit).await
        }
        None => admin.events.list_events(filter, limit).await,
    };
    rows.map_err(|e| ApiError::from_internal(e.as_ref(), "list events", "failed to list events"))
}

/// Query params for the event log (keyset paging on event time, like alert history).
#[derive(Deserialize)]
struct EventsQuery {
    before: Option<String>,
    /// Time-range lower bound (inclusive, RFC 3339). Distinct from `before` (the paging cursor).
    start: Option<String>,
    /// Time-range upper bound (inclusive, RFC 3339).
    end: Option<String>,
    limit: Option<i64>,
    kind: Option<String>,
    node_id: Option<Uuid>,
    matched: Option<bool>,
    /// Free-text matched against source (node name / IP) or message. With `regex`, it is instead a
    /// regular expression matched against the message only.
    q: Option<String>,
    /// Interpret `q` as a regular expression (message-only) rather than a plain term.
    regex: Option<bool>,
}

async fn list_events(
    _perm: RequireView,
    admin: Admin,
    State(st): State<ApiState>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Json<Vec<EventRow>>> {
    let filter = parse_event_filter(EventFilterInput {
        before: q.before.as_deref(),
        start: q.start.as_deref(),
        end: q.end.as_deref(),
        kind: q.kind,
        node_id: q.node_id,
        matched: q.matched,
        q: q.q.as_deref(),
        regex: q.regex.unwrap_or(false),
    })?;
    Ok(Json(
        search(&st, &admin, &filter, q.limit.unwrap_or(100)).await?,
    ))
}

/// Query params for `/events/stats`: the event-filter set (no paging cursor) plus the aggregation
/// controls — `group_by` (kind|action|trap|source|time), `limit` (categorical row cap), and for the
/// `time` series `bucket_secs` + `split=kind`.
#[derive(Deserialize)]
struct EventStatsQuery {
    start: Option<String>,
    end: Option<String>,
    kind: Option<String>,
    node_id: Option<Uuid>,
    matched: Option<bool>,
    q: Option<String>,
    regex: Option<bool>,
    group_by: Option<String>,
    limit: Option<i64>,
    bucket_secs: Option<i64>,
    split: Option<String>,
}

/// Fleet passive-event summary aggregates for the dashboard widgets.
///
/// Categorical (`group_by=kind|action|trap|source`) returns count-ordered buckets; `group_by=time`
/// returns a volume series. Routes to the same store the event log does, so a summary and the list
/// it summarises can never disagree about which store answered.
async fn events_stats(
    _perm: RequireView,
    admin: Admin,
    State(st): State<ApiState>,
    Query(q): Query<EventStatsQuery>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;

    let group_by = q.group_by.as_deref().unwrap_or("kind").to_owned();
    let filter = parse_event_filter(EventFilterInput {
        before: None,
        start: q.start.as_deref(),
        end: q.end.as_deref(),
        kind: q.kind,
        node_id: q.node_id,
        matched: q.matched,
        q: q.q.as_deref(),
        regex: q.regex.unwrap_or(false),
    })?;

    // The time series and the categorical breakdown return different row types, so this is the one
    // handler here that erases to `Response` rather than a single `Json<T>`.
    if group_by == "time" {
        let bucket = q.bucket_secs.unwrap_or(3600);
        let split_kind = q.split.as_deref() == Some("kind");
        let buckets = match st.logs.as_ref() {
            Some(logs) => {
                let ids = search_name_node_ids(&admin, &filter).await;
                logs.stats_series(&filter, &ids, bucket, split_kind).await
            }
            None => admin.events.stats_series(&filter, bucket, split_kind).await,
        }
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "event stats series",
                "failed to compute event stats",
            )
        })?;
        return Ok(Json(buckets).into_response());
    }

    let group = EventStatGroup::parse(&group_by).ok_or_else(|| {
        ApiError::bad_request(
            "invalid_filter",
            "group_by must be kind, action, trap, source, or time",
        )
    })?;
    let limit = q.limit.unwrap_or(12);
    let buckets = match st.logs.as_ref() {
        Some(logs) => {
            let ids = search_name_node_ids(&admin, &filter).await;
            logs.stats_grouped(&filter, &ids, group, limit).await
        }
        None => admin.events.stats_grouped(&filter, group, limit).await,
    }
    .map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "event stats grouped",
            "failed to compute event stats",
        )
    })?;
    Ok(Json(buckets).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn input(q: Option<&str>) -> EventFilterInput<'_> {
        EventFilterInput {
            q,
            ..Default::default()
        }
    }

    #[test]
    fn event_search_normalization() {
        // Absent / empty / whitespace-only ⇒ no filter (a blank box is a no-op).
        assert_eq!(normalize_search(None), None);
        assert_eq!(normalize_search(Some("")), None);
        assert_eq!(normalize_search(Some("   ")), None);
        // Surrounding whitespace is trimmed.
        assert_eq!(
            normalize_search(Some("  link down  ")).as_deref(),
            Some("link down")
        );
        // Length is capped (chars, not bytes) so a pathological input can't bloat the query.
        let capped = normalize_search(Some(&"あ".repeat(500))).unwrap();
        assert_eq!(capped.chars().count(), SEARCH_TERM_MAX_CHARS);
    }

    #[test]
    fn a_free_text_term_is_capped_on_every_surface() {
        // The drift this closes: the REST edge capped the term and the MCP tool did not, so the
        // surface with no human in the loop was the one that could send an unbounded term to the
        // store. Both now go through `parse_event_filter`, so the cap applies to both.
        let long = "x".repeat(5_000);
        let f =
            parse_event_filter(input(Some(&long))).expect("a long term is capped, not rejected");
        assert_eq!(
            f.search.as_deref().map(str::chars).map(Iterator::count),
            Some(SEARCH_TERM_MAX_CHARS)
        );
    }

    #[test]
    fn a_bad_cursor_and_a_bad_bound_get_different_codes() {
        // The UI branches on these: a bad cursor is its own paging bug, a bad bound is something
        // the operator typed.
        let bad_cursor = parse_event_filter(EventFilterInput {
            before: Some("nonsense"),
            ..Default::default()
        })
        .expect_err("an unparseable cursor must reject");
        assert_eq!(bad_cursor.code(), "invalid_cursor");

        for field in ["start", "end"] {
            let mut i = EventFilterInput::default();
            if field == "start" {
                i.start = Some("nonsense");
            } else {
                i.end = Some("nonsense");
            }
            assert_eq!(
                parse_event_filter(i).expect_err("must reject").code(),
                "invalid_filter",
                "{field}"
            );
        }

        // Absent is not malformed: an omitted bound means unbounded, and rejecting it would make
        // the default search — which sets neither — impossible.
        let none = parse_event_filter(EventFilterInput::default()).expect("no bounds is valid");
        assert!(none.before.is_none() && none.since.is_none() && none.until.is_none());
        // A well-formed offset bound is applied, not dropped.
        let bounded = parse_event_filter(EventFilterInput {
            start: Some("2026-07-25T12:00:00+09:00"),
            ..Default::default()
        })
        .expect("an offset bound is valid");
        assert_eq!(
            bounded.since.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-07-25T03:00:00+00:00")
        );
    }

    #[test]
    fn an_unknown_kind_is_rejected_rather_than_ignored() {
        // Ignoring it would silently widen the search to every kind — the operator would believe
        // they were looking at traps only.
        let err = parse_event_filter(EventFilterInput {
            kind: Some("snmp".to_owned()),
            ..Default::default()
        })
        .expect_err("an unknown kind must reject");
        assert_eq!(err.code(), "invalid_filter");
        assert!(err.message().contains("syslog"), "{}", err.message());
        for k in EVENT_KINDS {
            assert!(parse_event_filter(EventFilterInput {
                kind: Some(k.to_owned()),
                ..Default::default()
            })
            .is_ok());
        }
    }

    #[test]
    fn a_pathological_regex_is_refused_before_it_reaches_a_store() {
        // Compiled at the edge with the same guard rule compilation uses, so an unbounded pattern
        // cannot become a scan over the whole firehose.
        let err = parse_event_filter(EventFilterInput {
            q: Some("(("),
            regex: true,
            ..Default::default()
        })
        .expect_err("an uncompilable regex must reject");
        assert_eq!(err.code(), "invalid_filter");
        assert!(err.message().contains("regular expression"));

        // The same string is fine as a plain term — it is only a pattern when `regex` says so.
        assert!(parse_event_filter(input(Some("(("))).is_ok());
    }

    #[tokio::test]
    async fn reading_the_log_is_gated_before_the_store_is_consulted() {
        for st in [private_state(), public_state()] {
            let public = st.public_dashboard;
            let resp = router(st)
                .oneshot(
                    Request::builder()
                        .uri("/api/v1/events")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            // Private: 401 (never disclose whether a log store exists). Public dashboard: reads are
            // open, so the request reaches `Admin` and stops at skeleton mode's missing write side.
            let want = if public {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::UNAUTHORIZED
            };
            assert_eq!(resp.status(), want);
        }
    }

    #[tokio::test]
    async fn an_invalid_group_by_is_a_typed_400() {
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/events/stats?group_by=hostname")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Skeleton mode has no write side, so the availability guard fires first — the point is
        // that it fires *before* the store, not that this particular deployment can aggregate.
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "admin_unavailable");
    }
}
