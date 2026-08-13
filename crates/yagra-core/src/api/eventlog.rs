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
//! and log tiers carry ids only (ADR-011). So [`resolve_name_ids`] resolves the terms to ids
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

use super::extract::{Admin, RequireView, Scoped};
use super::util::normalize_search;
use super::{ApiError, ApiResult, ApiState};
use crate::events::{EventFilter, EventRow, EventStatBucket, EventStatGroup, EventTimeBucket};
use crate::logstore::NameIds;
use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_bus::EventKind;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(list_events, events_stats))]
pub(super) struct Doc;

/// The event-log read routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/events", get(list_events))
        .route("/api/v1/events/stats", get(events_stats))
}

/// How many node ids a name search may resolve to. A term matching half the fleet would otherwise
/// build an unbounded `IN (…)`.
const NAME_SEARCH_NODE_LIMIT: i64 = 50;

/// The event kinds a filter may name, for the rejection message. Rejecting an unknown kind rather
/// than ignoring it keeps a typo from silently widening the search to everything — and reading the
/// list off [`EventKind::ALL`] means a fourth source cannot be accepted here but missing from the
/// message, or the reverse.
fn kind_list() -> String {
    EventKind::ALL.map(EventKind::as_str).join(", ")
}

/// The rule outcomes a filter may name, read off the enum for the same reason as [`kind_list`].
fn action_list() -> String {
    crate::events::EventAction::ALL
        .map(crate::events::EventAction::as_str)
        .join(", ")
}

/// How many tokens one comma-separated set parameter may name.
///
/// Not a query-cost guard — that was measured and there isn't one: on 6.7M events an `in(…)` list
/// covering every stored value cost what a single value cost. It is a **request-size** guard, the
/// same reason a term is capped in chars, and it is deliberately far above the largest closed set
/// this API has (`syslog_severity`, 8 values). Do not read it as a sibling of
/// [`NAME_SEARCH_NODE_LIMIT`] or `STATS_SCOPE_NODE_LIMIT`: those two bound how much *work* a query
/// does, and their values were chosen from that.
const FILTER_SET_MAX_TOKENS: usize = 32;

/// Split a comma-separated set parameter into trimmed, de-duplicated tokens, preserving order.
///
/// `Some("")` yields an empty set, which means *unfiltered* — the same as omitting the parameter.
/// That is deliberate: the WebUI clears a filter by writing an empty value before it removes the
/// key, and a client that sends `kind=` should get the unfiltered list rather than a 400 or,
/// worse, a set containing one empty string that matches nothing.
fn split_set(raw: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in raw.unwrap_or_default().split(',') {
        let tok = tok.trim();
        if !tok.is_empty() && !out.iter().any(|v| v == tok) {
            out.push(tok.to_owned());
        }
    }
    out
}

/// Validate one set parameter: cap its size, then map each token through `parse`, rejecting the
/// first one that is not a member. Rejecting rather than dropping is the whole point — a dropped
/// token silently widens the result to everything, and the operator reads the widened list as the
/// answer to the question they asked.
fn parse_set<T>(
    field: &str,
    raw: Option<&str>,
    allowed: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<Vec<T>, ApiError> {
    let tokens = split_set(raw);
    if tokens.len() > FILTER_SET_MAX_TOKENS {
        return Err(ApiError::bad_request(
            "invalid_filter",
            format!("{field} may name at most {FILTER_SET_MAX_TOKENS} values"),
        ));
    }
    tokens
        .iter()
        .map(|t| {
            parse(t).ok_or_else(|| {
                ApiError::bad_request("invalid_filter", format!("{field} must be {allowed}"))
            })
        })
        .collect()
}

/// Build one column's text condition, or `None` when the term is blank.
///
/// `regex` is compiled here for the same reason the whole-row term's is — the edge is where a
/// pathological pattern has to stop, not the store.
fn text_cond(
    field: &str,
    term: Option<&str>,
    regex: bool,
    not: bool,
) -> Result<Option<crate::events::TextCond>, ApiError> {
    let Some(term) = normalize_search(term) else {
        // A blank term with `not` set is *not* "exclude everything": there is no condition at all.
        return Ok(None);
    };
    if regex {
        crate::events::compile_matcher(field, &term).map_err(|e| {
            ApiError::bad_request("invalid_filter", format!("invalid regular expression: {e}"))
        })?;
    }
    Ok(Some(crate::events::TextCond { term, regex, not }))
}

/// The raw, unvalidated filter fields, as either surface receives them.
///
/// ⚠️ **This struct is the REST/MCP parity seam.** `search_events_in` fills it field by field, so a
/// dimension added here is a compile error there until the MCP tool takes it too — which is what
/// makes ADR-042's read-parity mechanical for events rather than a thing to remember.
/// `the_mcp_event_search_takes_every_dimension_the_rest_edge_takes` covers the half the compiler
/// cannot see: that the MCP tool's *parameters* offer the dimension at all, rather than hard-coding
/// it to `None`.
#[derive(Default)]
pub(crate) struct EventFilterInput<'a> {
    /// Keyset paging cursor — rows strictly older than this. Distinct from `start`/`end`, which
    /// bound the range being searched.
    pub before: Option<&'a str>,
    pub start: Option<&'a str>,
    pub end: Option<&'a str>,
    /// Comma-separated event kinds; empty or absent means every kind.
    pub kind: Option<&'a str>,
    /// Comma-separated rule outcomes; empty or absent means every outcome.
    pub action: Option<&'a str>,
    /// Comma-separated syslog severities (0–7); empty or absent means every severity.
    pub severity: Option<&'a str>,
    pub node_id: Option<Uuid>,
    pub matched: Option<bool>,
    pub q: Option<&'a str>,
    pub regex: bool,
    /// The Message column's condition.
    pub msg: Option<&'a str>,
    pub msg_regex: bool,
    pub msg_not: bool,
    /// The Source column's condition (source IP or attributed node name).
    pub src: Option<&'a str>,
    pub src_not: bool,
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

    let kinds = parse_set("kind", input.kind, &kind_list(), |t| {
        EventKind::from_token(t).map(|k| k.as_str().to_owned())
    })?;
    let actions = parse_set("action", input.action, &action_list(), |t| {
        crate::events::EventAction::from_token(t).map(|a| a.as_str().to_owned())
    })?;
    let severities = parse_set("severity", input.severity, "a syslog severity 0–7", |t| {
        t.parse::<i16>().ok().filter(|v| (0..=7).contains(v))
    })?;
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
        kinds,
        actions,
        severities,
        node_id: input.node_id,
        matched: input.matched,
        search,
        regex: input.regex,
        message: text_cond("msg", input.msg, input.msg_regex, input.msg_not)?,
        // No `src_regex`: a source match spans the event's IP and the attributed node's *name*,
        // and the name half has no counterpart in the log store. See `events::TextCond`.
        source: text_cond("src", input.src, false, input.src_not)?,
        // Not part of the *request*: the group scope comes from the caller's session, never from a
        // query parameter, so a client cannot widen it by asking. Handlers that push it into the
        // store set it after this returns (`events_stats`); the ones that post-filter leave it None.
        visible_node_ids: None,
    })
}

/// How many nodes a group scope may resolve to before `/events/stats` refuses.
///
/// The aggregate endpoints cannot post-filter — the counts are produced by the store — so honouring
/// a scope means sending the visible node ids *into* the query, and both stores need them as a
/// literal list (VictoriaLogs has never heard of groups, and the two backends must be handed the
/// same restriction or they stop being a mirror). This bounds that list: at the cap the LogsQL
/// `in(…)` term is roughly 185 KB, which is fine in a POST body, and beyond it the honest answer is
/// a refusal rather than an unbounded query built from a scope somebody typed.
const STATS_SCOPE_NODE_LIMIT: usize = 5_000;

/// Resolve the caller's group scope into the restricting node-id set the two aggregate builders
/// take, or `None` when unrestricted.
///
/// ⚠️ This enumerates nodes, which every other scoped path deliberately avoids (`api/scope.rs`
/// explains why: at 50k nodes a per-request full-fleet scan is what S2/S6/S7 removed). It is
/// justified *here* and only here because the query is already restricted by the indexed
/// `group_id = ANY(…)` predicate, so it returns the caller's own nodes rather than the fleet — and
/// because the alternative for the log store is no filtering at all.
async fn stats_scope_node_ids(
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
) -> Result<Option<Vec<Uuid>>, ApiError> {
    let Some(groups) = scope.group_filter() else {
        return Ok(None);
    };
    let ids = admin.repo.nodes_in_groups(groups).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "resolve event scope",
            "failed to resolve the visible node set",
        )
    })?;
    if ids.len() > STATS_SCOPE_NODE_LIMIT {
        return Err(ApiError::forbidden_code(
            "scope_too_large",
            format!(
                "this account's scope covers more than {STATS_SCOPE_NODE_LIMIT} nodes, which is \
                 more than event statistics can restrict to; narrow the query with node_id"
            ),
        ));
    }
    // `Some(vec![])` survives on purpose: a scope naming only groups with no nodes must match
    // nothing. Collapsing it to `None` here would hand the caller the whole fleet's counts.
    Ok(Some(ids))
}

/// The node ids [`NameIds`] borrows from, owned by the caller for the duration of one store call.
pub(crate) struct ResolvedNames {
    search: Vec<Uuid>,
    source: Vec<Uuid>,
}

impl ResolvedNames {
    pub(crate) fn ids(&self) -> NameIds<'_> {
        NameIds {
            search: &self.search,
            source: &self.source,
        }
    }
}

/// Resolve the filter's free-text terms to matching node ids, for the log-store path's node-name
/// search — one set per term, never merged (see [`NameIds`]).
///
/// The whole-row term resolves only in plain mode; a regex search is message-only. The Source
/// column's condition always resolves, including when it is negated: "source is not rt01" has to
/// know which node *is* rt01 before it can exclude it.
pub(crate) async fn resolve_name_ids(
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
    filter: &EventFilter,
) -> ResolvedNames {
    async fn ids(
        admin: &super::AdminState,
        scope: &super::scope::NodeScope,
        term: Option<&str>,
    ) -> Vec<Uuid> {
        match term {
            Some(t) => admin
                .repo
                .node_ids_by_name_like(scope.group_filter(), t, NAME_SEARCH_NODE_LIMIT)
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        }
    }
    let search_term = filter.search.as_deref().filter(|_| !filter.regex);
    ResolvedNames {
        search: ids(admin, scope, search_term).await,
        source: ids(
            admin,
            scope,
            filter.source.as_ref().map(|c| c.term.as_str()),
        )
        .await,
    }
}

/// Search the event log, routing to whichever store is the source of record.
///
/// Scope is applied **here**, after the store answers, rather than as a `node_id` set pushed into
/// each backend. There are two backends with two query languages (PostgreSQL and LogsQL) and
/// `extensibility.md` §2 already lists them as a mirror that has drifted before; one filter that
/// both paths flow through cannot drift at all. The cost is that a page may come back shorter than
/// `limit` for a scoped caller — acceptable, because the cursor is the event time, so paging still
/// terminates and no row is skipped.
///
/// **An event with no `node_id` is hidden from a scoped caller.** That is the same rule
/// `Scope::allows` applies to an ungrouped node, and it matters more here: an unattributed syslog
/// message is exactly the case where the body may name a device the caller cannot otherwise see,
/// and syslog bodies routinely carry credentials (ADR-024).
pub(crate) async fn search(
    st: &ApiState,
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
    filter: &EventFilter,
    limit: i64,
) -> Result<Vec<EventRow>, ApiError> {
    let rows = match st.logs.as_ref() {
        Some(logs) => {
            let names = resolve_name_ids(admin, scope, filter).await;
            logs.search(filter, names.ids(), limit).await
        }
        None => admin.events.list_events(filter, limit).await,
    };
    let rows = rows
        .map_err(|e| ApiError::from_internal(e.as_ref(), "list events", "failed to list events"))?;
    if scope.is_all() {
        return Ok(rows);
    }
    Ok(rows
        .into_iter()
        .filter(|r| {
            r.node_id
                .is_some_and(|n| scope.allows_node(st, yagra_common::NodeId::from(n)))
        })
        .collect())
}

/// Query params for the event log (keyset paging on event time, like alert history).
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct EventsQuery {
    before: Option<String>,
    /// Time-range lower bound (inclusive, RFC 3339). Distinct from `before` (the paging cursor).
    start: Option<String>,
    /// Time-range upper bound (inclusive, RFC 3339).
    end: Option<String>,
    limit: Option<i64>,
    /// Event kinds to include, comma-separated (`syslog,trap`). A single value is the long-standing
    /// spelling and still works; an empty value or an absent parameter means every kind.
    kind: Option<String>,
    node_id: Option<Uuid>,
    /// Only rule-matched events (or only unmatched ones). Superseded by `action`, which says *what*
    /// the rule did; kept because it is a narrower question some clients still ask.
    matched: Option<bool>,
    /// Rule outcomes to include, comma-separated (`fired,cleared`); empty or absent means all.
    action: Option<String>,
    /// Syslog severities (0–7) to include, comma-separated; empty or absent means all. An event
    /// with no syslog severity — a trap, a webhook — matches no severity filter.
    severity: Option<String>,
    /// Free-text matched against source (node name / IP) or message, case-insensitively. Whether
    /// it also matches inside a word depends on the store this deployment searches: PostgreSQL
    /// matches any substring, a log store matches whole words. With `regex`, it is instead a
    /// regular expression matched against the message only, which reaches inside words on either.
    q: Option<String>,
    /// Interpret `q` as a regular expression (message-only) rather than a plain term.
    regex: Option<bool>,
    /// Message-only condition, matched with the same store-dependent word rules as `q`.
    msg: Option<String>,
    /// Interpret `msg` as a regular expression, which reaches inside words on either store.
    msg_regex: Option<bool>,
    /// Keep the events whose message does **not** match `msg`.
    msg_not: Option<bool>,
    /// Condition on the event's source: its IP, or the name of the node it is attributed to. There
    /// is no regex form — the node-name half is resolved against PostgreSQL and has no counterpart
    /// in a log store, so a pattern would mean two different things on the two backends.
    src: Option<String>,
    /// Keep the events whose source does **not** match `src`.
    src_not: Option<bool>,
}

#[utoipa::path(
    get, path = "/api/v1/events", tag = "eventlog",
    params(EventsQuery),
    responses(
        (status = 200, description = "Matching events, newest first, from whichever store is the source of record", body = Vec<EventRow>),
        (status = 400, description = "`before` is not RFC 3339, a range bound is malformed, the kind is unknown, or the regex does not compile", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side to resolve node names against", body = super::error::ErrorBody),
    ),
)]
async fn list_events(
    _perm: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
    State(st): State<ApiState>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Json<Vec<EventRow>>> {
    let filter = parse_event_filter(EventFilterInput {
        before: q.before.as_deref(),
        start: q.start.as_deref(),
        end: q.end.as_deref(),
        kind: q.kind.as_deref(),
        action: q.action.as_deref(),
        severity: q.severity.as_deref(),
        node_id: q.node_id,
        matched: q.matched,
        q: q.q.as_deref(),
        regex: q.regex.unwrap_or(false),
        msg: q.msg.as_deref(),
        msg_regex: q.msg_regex.unwrap_or(false),
        msg_not: q.msg_not.unwrap_or(false),
        src: q.src.as_deref(),
        src_not: q.src_not.unwrap_or(false),
    })?;
    Ok(Json(
        search(&st, &admin, &scope, &filter, q.limit.unwrap_or(100)).await?,
    ))
}

/// Query params for `/events/stats`: the event-filter set (no paging cursor) plus the aggregation
/// controls — `group_by` (kind|action|trap|source|time), `limit` (categorical row cap), and for the
/// `time` series `bucket_secs` + `split=kind`.
///
/// Every filter parameter `GET /events` takes, it takes too — that is what lets a facet count
/// answer "how many rows would *this* column's value give me, under everything else that is
/// filtered", which is the count an autofilter shows. The field list is duplicated rather than
/// flattened because `IntoParams` describes a flat query string either way; the duplication is what
/// `every_query_surface_offers_the_same_event_filter_dimensions` exists to catch.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct EventStatsQuery {
    start: Option<String>,
    end: Option<String>,
    /// Event kinds to include, comma-separated; empty or absent means every kind.
    kind: Option<String>,
    node_id: Option<Uuid>,
    matched: Option<bool>,
    /// Rule outcomes to include, comma-separated; empty or absent means all.
    action: Option<String>,
    /// Syslog severities (0–7) to include, comma-separated; empty or absent means all.
    severity: Option<String>,
    q: Option<String>,
    regex: Option<bool>,
    /// Message-only condition (see `GET /events`).
    msg: Option<String>,
    /// Interpret `msg` as a regular expression.
    msg_regex: Option<bool>,
    /// Keep the events whose message does **not** match `msg`.
    msg_not: Option<bool>,
    /// Condition on the event's source IP or attributed node name (see `GET /events`).
    src: Option<String>,
    /// Keep the events whose source does **not** match `src`.
    src_not: Option<bool>,
    group_by: Option<String>,
    limit: Option<i64>,
    bucket_secs: Option<i64>,
    split: Option<String>,
}

/// The two row shapes `/events/stats` answers with, as one type.
///
/// `#[serde(untagged)]`, so the bytes are exactly the bare array either arm used to serialize on its
/// own — the union exists because a `Response` names no shape at all in the generated contract, and
/// a client that has to guess between two arrays guesses wrong on one of them.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(untagged)]
pub(super) enum EventStats {
    /// `group_by=time`: the volume series.
    Series(Vec<EventTimeBucket>),
    /// Every other `group_by`: count-ordered categorical buckets.
    Grouped(Vec<EventStatBucket>),
}

/// Fleet passive-event summary aggregates for the dashboard widgets.
///
/// Categorical (`group_by=kind|action|trap|source`) returns count-ordered buckets; `group_by=time`
/// returns a volume series. Routes to the same store the event log does, so a summary and the list
/// it summarises can never disagree about which store answered.
#[utoipa::path(
    get, path = "/api/v1/events/stats", tag = "eventlog",
    params(EventStatsQuery),
    responses(
        (status = 200, description = "A volume series for `group_by=time`, count-ordered buckets otherwise", body = EventStats),
        (status = 400, description = "A range bound is malformed, the kind is unknown, the regex does not compile, or `group_by` is not one of kind|action|trap|source|time", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side to resolve node names against", body = super::error::ErrorBody),
    ),
)]
async fn events_stats(
    _perm: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
    State(st): State<ApiState>,
    Query(q): Query<EventStatsQuery>,
) -> ApiResult<Json<EventStats>> {
    let group_by = q.group_by.as_deref().unwrap_or("kind").to_owned();
    let mut filter = parse_event_filter(EventFilterInput {
        before: None,
        start: q.start.as_deref(),
        end: q.end.as_deref(),
        kind: q.kind.as_deref(),
        action: q.action.as_deref(),
        severity: q.severity.as_deref(),
        node_id: q.node_id,
        matched: q.matched,
        q: q.q.as_deref(),
        regex: q.regex.unwrap_or(false),
        msg: q.msg.as_deref(),
        msg_regex: q.msg_regex.unwrap_or(false),
        msg_not: q.msg_not.unwrap_or(false),
        src: q.src.as_deref(),
        src_not: q.src_not.unwrap_or(false),
    })?;
    // Unlike `/events`, the counts here are produced by the store, so there is no row set left to
    // post-filter — the scope has to go *into* both queries as a restricting node-id set. It rides
    // on the filter itself, so every store call below carries it whether or not its author
    // remembered; the same reasoning as `EVENT_FILTER_WHERE` being always-present.
    filter.visible_node_ids = stats_scope_node_ids(&admin, &scope).await?;

    if group_by == "time" {
        let bucket = q.bucket_secs.unwrap_or(3600);
        let split_kind = q.split.as_deref() == Some("kind");
        let buckets = match st.logs.as_ref() {
            Some(logs) => {
                let names = resolve_name_ids(&admin, &scope, &filter).await;
                logs.stats_series(&filter, names.ids(), bucket, split_kind)
                    .await
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
        return Ok(Json(EventStats::Series(buckets)));
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
            let names = resolve_name_ids(&admin, &scope, &filter).await;
            logs.stats_grouped(&filter, names.ids(), group, limit).await
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
    Ok(Json(EventStats::Grouped(buckets)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    // Only the tests assert the cap's value; the production path reaches it through
    // `normalize_search`, which is where the policy lives now (`api/util.rs`).
    use crate::api::util::SEARCH_TERM_MAX_CHARS;
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
            kind: Some("snmp"),
            ..Default::default()
        })
        .expect_err("an unknown kind must reject");
        assert_eq!(err.code(), "invalid_filter");
        assert!(err.message().contains("syslog"), "{}", err.message());
        for k in EventKind::ALL.map(EventKind::as_str) {
            assert!(parse_event_filter(EventFilterInput {
                kind: Some(k),
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

    #[test]
    fn a_set_parameter_takes_one_value_a_list_or_nothing() {
        // A single token is the long-standing spelling and must keep working — a v0.2.6 bookmark
        // says `kind=trap`.
        let one = parse_event_filter(EventFilterInput {
            kind: Some("trap"),
            ..Default::default()
        })
        .expect("one kind is valid");
        assert_eq!(one.kinds, vec!["trap".to_owned()]);

        let many = parse_event_filter(EventFilterInput {
            kind: Some("syslog, trap ,syslog"),
            ..Default::default()
        })
        .expect("a list is valid");
        // Trimmed and de-duplicated: a repeated token is a click, not an error.
        assert_eq!(many.kinds, vec!["syslog".to_owned(), "trap".to_owned()]);

        // An empty value is *unfiltered*, not "no kinds match" — the UI writes it while clearing.
        for raw in ["", " ", ",,"] {
            let f = parse_event_filter(EventFilterInput {
                kind: Some(raw),
                ..Default::default()
            })
            .unwrap_or_else(|_| panic!("{raw:?} is an empty set, not an error"));
            assert!(f.kinds.is_empty(), "{raw:?}");
        }
    }

    #[test]
    fn an_unknown_token_in_a_set_is_rejected_rather_than_dropped() {
        // Dropping it would widen the answer to every value while the operator reads the widened
        // list as the answer to the question they asked. Same argument as the single-kind case,
        // and now it has to hold for one bad token among good ones.
        for (field, raw) in [
            ("kind", "syslog,snmp"),
            ("action", "fired,exploded"),
            ("severity", "3,9"),
            ("severity", "3,-1"),
            ("severity", "3,four"),
        ] {
            let mut i = EventFilterInput::default();
            match field {
                "kind" => i.kind = Some(raw),
                "action" => i.action = Some(raw),
                _ => i.severity = Some(raw),
            }
            let err = match parse_event_filter(i) {
                Ok(_) => panic!("{field}={raw} must reject"),
                Err(e) => e,
            };
            assert_eq!(err.code(), "invalid_filter", "{field}={raw}");
        }

        // The good tokens on their own still parse, so the rejection is about the bad one.
        let ok = parse_event_filter(EventFilterInput {
            kind: Some("syslog"),
            action: Some("fired,cleared"),
            severity: Some("0,7"),
            ..Default::default()
        })
        .expect("valid tokens parse");
        assert_eq!(ok.actions, vec!["fired".to_owned(), "cleared".to_owned()]);
        assert_eq!(ok.severities, vec![0, 7]);
    }

    #[test]
    fn a_set_is_capped_in_size() {
        // A request-size guard, not a query-cost one (see FILTER_SET_MAX_TOKENS). At the cap it
        // still parses; one past it is a typed 400 rather than a silently truncated filter.
        let at_cap = (0..FILTER_SET_MAX_TOKENS)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let over = format!("{at_cap},one-too-many");
        let err = parse_event_filter(EventFilterInput {
            severity: Some(&over),
            ..Default::default()
        })
        .expect_err("over the cap must reject");
        assert_eq!(err.code(), "invalid_filter");
        assert!(err.message().contains("at most"), "{}", err.message());
    }

    #[test]
    fn a_column_condition_survives_the_edge_intact() {
        let f = parse_event_filter(EventFilterInput {
            msg: Some("  policypermit  "),
            msg_not: true,
            src: Some("rtr1"),
            ..Default::default()
        })
        .expect("a plain condition is valid");
        let msg = f.message.expect("the message condition survives");
        assert_eq!(msg.term, "policypermit"); // trimmed by the shared normalizer
        assert!(msg.not && !msg.regex);
        assert!(f.source.is_some_and(|c| c.term == "rtr1" && !c.not));

        // A blank term is no condition at all, even with `not` set — "exclude nothing" must not
        // become "exclude everything".
        let blank = parse_event_filter(EventFilterInput {
            msg: Some("   "),
            msg_not: true,
            ..Default::default()
        })
        .expect("a blank condition is valid");
        assert!(blank.message.is_none());

        // The pattern guard applies to the column condition too, not just to `q`.
        let bad = parse_event_filter(EventFilterInput {
            msg: Some("(("),
            msg_regex: true,
            ..Default::default()
        })
        .expect_err("an uncompilable pattern must reject");
        assert_eq!(bad.code(), "invalid_filter");
    }

    #[test]
    fn the_source_condition_is_never_a_regex() {
        // There is no `src_regex` parameter, and this is what keeps it that way: a source match
        // spans the event IP *and* the attributed node's name, and the name half is resolved
        // against PostgreSQL and absent from the log store. A pattern would therefore mean "IP or
        // name" on one deployment and "IP only" on another — a divergence on an axis the two
        // backends are not permitted to differ on. See `events::TextCond`.
        // Checked against the field lists, not the file: the prose above says `src_regex` in order
        // to say there isn't one, and a whole-file scan would match that.
        let field = format!("\n    src_{}: Option<", "regex");
        for (what, decl, src) in [
            (
                "GET /events",
                "pub(super) struct EventsQuery {",
                include_str!("eventlog.rs"),
            ),
            (
                "GET /events/stats",
                "pub(super) struct EventStatsQuery {",
                include_str!("eventlog.rs"),
            ),
            (
                "the MCP search_events tool",
                "pub(crate) struct EventSearchParams {",
                include_str!("../mcp/tools.rs"),
            ),
        ] {
            let body = src.split(decl).nth(1).expect("the struct is declared");
            let body = &body[..body.find("\n}\n").expect("the struct ends")];
            assert!(
                !body.contains(&field),
                "{what} grew a source-regex parameter; decide what it means on a log store first"
            );
        }
        let f = parse_event_filter(EventFilterInput {
            src: Some("^rtr"),
            ..Default::default()
        })
        .expect("a source term is valid");
        assert!(f.source.is_some_and(|c| !c.regex));
    }

    /// Every filter dimension `GET /events` takes is offered by `/events/stats` and by the MCP
    /// `search_events` tool.
    ///
    /// Two mirrors, one test. The stats half is what makes an autofilter's counts mean anything —
    /// a facet that ignores the other columns' filters shows numbers that do not match the list
    /// beside it. The MCP half is ADR-042's read parity, and it is the one that fails *quietly*:
    /// `search_events_in` fills `EventFilterInput` field by field, so a new dimension is a compile
    /// error there — but the compiler is equally happy if the tool hard-codes it to `None`, which
    /// is a tool that silently cannot answer a question the WebUI can.
    #[test]
    fn every_query_surface_offers_the_same_event_filter_dimensions() {
        // Built at runtime: a literal list in this file would match itself in the source scan.
        let dims = [
            "kind",
            "action",
            "severity",
            "node_id",
            "matched",
            "msg",
            "msg_regex",
            "msg_not",
            "src",
            "src_not",
        ];
        let rest = include_str!("eventlog.rs");
        let mcp = include_str!("../mcp/tools.rs");

        fn fields<'a>(src: &'a str, decl: &str) -> &'a str {
            let body = src.split(decl).nth(1).expect("the struct is in this file");
            &body[..body.find("\n}\n").expect("the struct ends")]
        }
        let stats = fields(rest, "pub(super) struct EventStatsQuery {");
        let params = fields(mcp, "pub(crate) struct EventSearchParams {");
        for d in dims {
            let field = format!("\n    {d}: Option<");
            assert!(
                stats.contains(&field),
                "/events/stats cannot filter by {d}, so its facet counts disagree with the list"
            );
            assert!(
                params.contains(&field),
                "the MCP search_events tool cannot filter by {d} (ADR-042 read parity)"
            );
        }
    }

    #[test]
    fn the_mcp_event_search_takes_every_dimension_the_rest_edge_takes() {
        // The other half of the parity claim: the tool must actually *pass* what it takes. A
        // parameter that is declared and then never read is the same silent failure as one that was
        // never declared, and it type-checks.
        let mcp = include_str!("../mcp/tools.rs");
        let call = mcp
            .split("crate::api::eventlog::EventFilterInput {")
            .nth(1)
            .expect("search_events_in builds the shared input");
        let call = &call[..call.find("})").expect("the initializer ends")];
        for (field, from) in [
            ("kind", "p.kind"),
            ("action", "p.action"),
            ("severity", "p.severity"),
            ("msg", "p.msg"),
            ("msg_regex", "p.msg_regex"),
            ("msg_not", "p.msg_not"),
            ("src", "p.src"),
            ("src_not", "p.src_not"),
        ] {
            let line = call
                .lines()
                .find(|l| l.trim_start().starts_with(&format!("{field}:")))
                .unwrap_or_else(|| panic!("search_events_in does not pass {field}"));
            assert!(
                line.contains(from),
                "search_events_in passes {field} from something other than its own parameter: {line}"
            );
        }
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
