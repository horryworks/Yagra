// SPDX-License-Identifier: AGPL-3.0-only
//! **The event query** — the predicate both backends implement, its binder, and the PostgreSQL
//! statements built on it (ADR-095).
//!
//! This is a face with three implementations, which is why it has a name of its own rather than
//! sitting inside [`super::repo`]: PostgreSQL here, LogsQL in [`crate::logstore`], and the
//! highlighter in `web/src/lib/matchRanges.ts`. The differences between them are deliberate and
//! documented at each clause (ADR-024/053); what must never differ is *what* they look for, which
//! is why [`EVENT_FILTER_WHERE`], [`AUTH_FAILURE_PHRASES`] and [`SIGNATURE_TIERS`] are
//! `pub(crate)` and read from the other side rather than restated there.
//!
//! `SIGNATURE_TIER_ACCESSORS` lives here too, beside [`SIGNATURE_TIERS`] rather than beside
//! [`super::PersistRecord`] whose fields it reads: it exists only to keep the SQL column list and
//! the in-memory twin in step, and a mirror with its halves in two files is the failure this module
//! is organised to prevent.

use chrono::{DateTime, Utc};
use serde::Serialize;

// The vocabulary lives in the parent, which a child can see without any widening — see
// `super`'s doc for why that is what decides where a thing goes here.
use super::*;

/// How many binds [`EVENT_FILTER_WHERE`] consumes.
///
/// Every consumer needs one more of its own — a page size, a row cap, a bucket width — and writes
/// it as `${EVENT_FILTER_BINDS + 1}` rather than as a literal. Widening the predicate used to mean
/// renumbering six unrelated `$10`s by hand across five builders, and a missed one is neither a
/// compile error nor a crash: PostgreSQL happily binds the page size into a filter slot and answers
/// a different question. `the_extra_bind_follows_the_predicate` pins the derivation.
pub(crate) const EVENT_FILTER_BINDS: usize = 16;

/// `$N` for a consumer's own trailing bind — see [`EVENT_FILTER_BINDS`].
fn extra_bind() -> String {
    format!("${}", EVENT_FILTER_BINDS + 1)
}

/// The shared `WHERE` predicate for the event log + summary-stats queries. Binds $1..=$16:
/// $1 before (paging cursor), $2 since, $3 until, $4 kinds, $5 node_id, $6 matched, $7 search,
/// $8 regex, $9 the RBAC visible-node restriction, $10 actions, $11 syslog severities, $12–$14 the
/// message condition (term / is-regex / negated), $15–$16 the source condition (term / negated).
/// Kept in one place so `list_events` and `/events/stats` filter identically (the dashboard
/// summaries must line up with the log). Uses the `e` (events) / `n` (nodes) aliases — every
/// consumer joins `nodes n` (the name search needs it).
///
/// The three set dimensions are spelled `cardinality(…) = 0 OR … = ANY(…)` rather than as a
/// nullable scalar, so "unfiltered" has exactly one representation (an empty array) on the wire,
/// in the struct and in the SQL. `e.syslog_severity` is nullable and `NULL = ANY(…)` is NULL, so a
/// trap is excluded by a severity filter — which is the honest answer, since it has no severity.
///
/// Negation is `<> $not`: boolean inequality is XOR, so one clause serves both directions. The
/// source disjunction is wrapped in `COALESCE(…, FALSE)` first — without it a row with no source IP
/// and no attributed node yields NULL, and a NULL cannot satisfy a *negated* condition it plainly
/// does not match.
///
/// The three time bounds are **event time** in epoch milliseconds, matching what the VictoriaLogs
/// builder filters on (`logstore::build_filter_part`) — see [`EventFilter`] for why. Callers bind
/// them with [`ms_bound`]. `events_at_idx` (migration 0055) serves the ordering and the range.
///
/// `$9` is the group-scope restriction, written as an always-present clause for the same reason as
/// `NodeRepo::SCOPE_PREDICATE`: a conditionally-appended one has a branch that can be forgotten, and
/// forgetting it fails **open**. Note it is ANDed at the top level, *after* the `$7` search
/// alternation — the search's `OR`s are already parenthesised, so it cannot be swallowed by them.
pub(crate) const EVENT_FILTER_WHERE: &str = "($1::bigint IS NULL OR e.at_unix_ms < $1) \
     AND ($2::bigint IS NULL OR e.at_unix_ms >= $2) \
     AND ($3::bigint IS NULL OR e.at_unix_ms <= $3) \
     AND (cardinality($4::text[]) = 0 OR e.kind = ANY($4)) \
     AND ($5::uuid IS NULL OR e.node_id = $5) \
     AND ($6::boolean IS NULL OR (e.matched_rule_id IS NOT NULL) = $6) \
     AND ($7::text IS NULL \
          OR ($8::boolean = FALSE AND (e.message ILIKE '%' || $7 || '%' \
                                       OR host(e.source_ip) ILIKE '%' || $7 || '%' \
                                       OR n.name ILIKE '%' || $7 || '%')) \
          OR ($8::boolean = TRUE AND e.message ~* $7)) \
     AND ($9::uuid[] IS NULL OR e.node_id = ANY($9)) \
     AND (cardinality($10::text[]) = 0 OR e.action = ANY($10)) \
     AND (cardinality($11::int2[]) = 0 OR e.syslog_severity = ANY($11)) \
     AND ($12::text IS NULL \
          OR (CASE WHEN $13::boolean THEN e.message ~* $12 \
                   ELSE e.message ILIKE '%' || $12 || '%' END) <> $14::boolean) \
     AND ($15::text IS NULL \
          OR (COALESCE(host(e.source_ip) ILIKE '%' || $15 || '%', FALSE) \
              OR COALESCE(n.name ILIKE '%' || $15 || '%', FALSE)) <> $16::boolean)";

/// The SNMP `authenticationFailure` trap OID (RFC 3418), the unambiguous half of the auth signal.
pub(crate) const AUTH_FAILURE_TRAP_OID: &str = "1.3.6.1.6.3.1.1.5.5";

/// Message phrases that mark an authentication failure in free-text syslog.
///
/// Shared by both backends so `auth_probe` asks the same question of PostgreSQL and VictoriaLogs.
/// The two still match them *differently* — SQL `ILIKE '%…%'` reaches inside a token, LogsQL
/// `i("…")` matches adjacent whole tokens case-insensitively — which is the same permitted axis the
/// free-text search documents, for the same measured reason. Keeping the phrases in one place at
/// least means the two never disagree about *what* they are looking for.
pub(crate) const AUTH_FAILURE_PHRASES: [&str; 4] = [
    "authentication fail",
    "auth failure",
    "login fail",
    "failed password",
];

/// The auth-signal predicate for the SQL side, built from the shared vocabulary above. Phrases are
/// compile-time constants, never request input, so interpolation here introduces no injection path.
fn auth_signal_predicate() -> String {
    let mut parts = vec![format!("e.trap_oid = '{AUTH_FAILURE_TRAP_OID}'")];
    parts.extend(
        AUTH_FAILURE_PHRASES
            .iter()
            .map(|p| format!("e.message ILIKE '%{p}%'")),
    );
    parts.join(" OR ")
}

/// How a plain (non-regex) search term matches on this deployment. Which one applies depends on
/// the store that answers event searches, so a client that wants a term to behave the same
/// everywhere should use a regular expression instead.
//  Kept short deliberately: a `ToSchema` doc comment is published verbatim to API clients and lands
//  in the public API reference, so the design rationale goes in `//` lines like these. There are two
//  reasons this is *reported* rather than merely documented, and the second is why it must never be
//  removed:
//
//  1. The empty result is otherwise unexplainable. `%%01POLICY/6/POLICYPERMIT` tokenizes to
//     `01policy` and `policypermit`, so searching `PERMIT` returns nothing on a log-store deployment
//     while the operator is looking at the letters on screen. "Nothing matches these filters" is a
//     true sentence that explains none of that. (`POLICY` *does* find it — the term is matched from
//     the start of a word since ADR-053 Inc.2d, which is why this value is `prefix` and not `token`.
//     The remaining gap is the middle and the end of a word.)
//  2. It is how a new WebUI detects an old core. axum's `Query<T>` drops unknown parameters
//     silently, so a WebUI carrying the ADR-053 filters, pointed at a core that predates them, would
//     have every new filter quietly do nothing. The presence of this field is the signal that the
//     core understands them; its absence makes the UI say less rather than guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventSearchSemantics {
    /// A term matches from the start of a word (a log store's inverted index). `POLICY` finds
    /// `POLICYPERMIT`, but `PERMIT` does not; the regex mode does.
    Prefix,
    /// A term matches any substring (PostgreSQL `ILIKE '%term%'`).
    Substring,
}

// No `as_str`/`ALL` here, unlike every other enum in this file. Those exist where a value is *both*
// a database column and a JSON field, produced by two mechanisms that nothing makes agree. This one
// is only ever serialized, so its `as_str` would have no production caller and the
// `token_and_serde_agree` pairing would be an indirection with one real side. What does need pinning
// is the spelling itself — the WebUI's `SearchSemantics` union hardcodes these two strings — and
// `the_search_semantics_spelling_is_what_the_webui_expects` does that directly.

/// A per-column text condition from the filter row (ADR-053): a term, how it is matched, and
/// whether the match is negated.
///
/// `regex` is **message-only**, and there is deliberately no `src_regex` parameter to set it on a
/// source condition. A source match covers the event's IP *and* the name of the node it is
/// attributed to; the node name lives in PostgreSQL's `nodes` join and has no counterpart in the
/// log store, which is handed resolved ids instead. A regex there would therefore mean "IP or name"
/// on one backend and "IP only" on the other — a divergence nobody asked for, on an axis the
/// backends are not permitted to differ on. `the_source_condition_is_never_a_regex` pins it.
#[derive(Debug, Clone, Default)]
pub struct TextCond {
    /// The term or pattern. Already trimmed and length-capped by the API edge.
    pub term: String,
    /// Interpret `term` as a regular expression rather than as a phrase/substring.
    pub regex: bool,
    /// Invert the match — the row is kept when the term does **not** match.
    pub not: bool,
}

/// A time bound as the epoch milliseconds [`EVENT_FILTER_WHERE`] compares against. One helper so
/// the three bounds can never be bound in different units.
fn ms_bound(at: Option<DateTime<Utc>>) -> Option<i64> {
    at.map(|t| t.timestamp_millis())
}

/// Bind `$1..=$16` of [`EVENT_FILTER_WHERE`] onto a query, in the one order that matches it.
///
/// One helper because the sequence is positional and silent when wrong: swapping `$5` and `$6`
/// still compiles, still runs, and just answers a different question. Every consumer of the shared
/// predicate binds through here so there is a single place the order is stated, and
/// `the_where_clause_binds_every_parameter_it_names` counts the two against each other.
pub(super) fn bind_event_filter<'q>(
    q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    filter: &'q EventFilter,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    // A condition contributes its term as NULL when absent, which is what switches its clause off.
    fn term(c: Option<&TextCond>) -> Option<&str> {
        c.map(|c| c.term.as_str())
    }
    q.bind(ms_bound(filter.before))
        .bind(ms_bound(filter.since))
        .bind(ms_bound(filter.until))
        .bind(&filter.kinds)
        .bind(filter.node_id)
        .bind(filter.matched)
        .bind(filter.search.as_deref())
        .bind(filter.regex)
        .bind(filter.visible_node_ids.as_deref())
        .bind(&filter.actions)
        .bind(&filter.severities)
        .bind(term(filter.message.as_ref()))
        .bind(filter.message.as_ref().is_some_and(|c| c.regex))
        .bind(filter.message.as_ref().is_some_and(|c| c.not))
        .bind(term(filter.source.as_ref()))
        .bind(filter.source.as_ref().is_some_and(|c| c.not))
}

/// Build the keyset-paged event-list SQL. Binds are the filter's, plus one for the page size.
///
/// Extracted like the two stats builders so the **ordering column** is assertable: it has to be the
/// one `EVENT_FILTER_WHERE`'s cursor compares against, and the same one VictoriaLogs sorts by, or
/// paging skips and repeats rows. See `logstore::tests::both_backends_filter_on_event_time_…`.
pub(super) fn list_events_sql() -> String {
    let limit = extra_bind();
    format!(
        "SELECT e.id, e.kind, e.at_unix_ms, e.recorded_at, host(e.source_ip) AS source_ip, \
                e.node_id, e.source_id, e.pool, e.facility, e.syslog_severity, e.hostname, \
                e.app_name, e.trap_oid, e.varbinds, e.message, e.matched_rule_id, e.action \
         FROM events e LEFT JOIN nodes n ON n.id = e.node_id \
         WHERE {EVENT_FILTER_WHERE} \
         ORDER BY e.at_unix_ms DESC LIMIT {limit}"
    )
}

// ── Troubleshoot analytics SQL (ADR-022) ────────────────────────────────────────────────────────
// Extracted like the two `/events/stats` builders so the shared predicate and the group-scope bind
// are assertable without a database — see `the_analytics_aggregates_share_the_event_filter_predicate`.
// Every one binds the filter, plus one more where it needs a cap or a bucket width.

/// Per-(node, bucket) event counts. Uncorrelated events are excluded — a storm has a device.
pub(super) fn agg_counts_by_bucket_sql() -> String {
    let secs = extra_bind();
    format!(
        "SELECT e.node_id, (e.at_unix_ms / 1000 / {secs}) * {secs} AS bucket, count(*) AS n \
         FROM events e LEFT JOIN nodes n ON n.id = e.node_id \
         WHERE {EVENT_FILTER_WHERE} AND e.node_id IS NOT NULL \
         GROUP BY e.node_id, bucket"
    )
}

/// Per-(node, syslog-severity) counts over syslog events.
pub(super) fn agg_severity_counts_sql() -> String {
    format!(
        "SELECT e.node_id, e.syslog_severity, count(*) AS n \
         FROM events e LEFT JOIN nodes n ON n.id = e.node_id \
         WHERE {EVENT_FILTER_WHERE} AND e.kind = 'syslog' AND e.node_id IS NOT NULL \
           AND e.syslog_severity IS NOT NULL \
         GROUP BY e.node_id, e.syslog_severity"
    )
}

/// The clustering key for an unmatched event, **most specific first**.
///
/// One list, both backends: the PostgreSQL `COALESCE` below and the LogsQL tier queries in
/// `logstore.rs` are generated from it, so PostgreSQL and VictoriaLogs cannot answer `rule_gap`
/// with different groupings — pinned by `the_two_backends_cluster_signatures_in_the_same_order`.
///
/// Why this order: a trap OID identifies the event class exactly. `signature` is the device's own
/// event code, recovered from the message text by `yagra_ingest::extract_signature` for the very
/// large class of senders whose datagrams parse as neither RFC 3164 nor RFC 5424 and therefore
/// carry no APP-NAME at all. `app_name` last — it is the coarsest of the three (a daemon name, not
/// an event class), and it is what rows written before the `signature` column existed still have.
pub(crate) const SIGNATURE_TIERS: [&str; 3] = ["trap_oid", "signature", "app_name"];

/// `COALESCE(e.trap_oid, e.signature, e.app_name)`, built from [`SIGNATURE_TIERS`].
fn signature_coalesce_sql() -> String {
    let cols: Vec<String> = SIGNATURE_TIERS.iter().map(|f| format!("e.{f}")).collect();
    format!("COALESCE({})", cols.join(", "))
}

/// Top unmatched signatures, clustered by [`SIGNATURE_TIERS`].
///
/// `pub(crate)` only so `logstore::the_two_backends_cluster_signatures_in_the_same_order` can read
/// it — the same reason [`EVENT_FILTER_WHERE`] is. It is a pure string builder; nothing else calls it.
pub(crate) fn agg_unmatched_signatures_sql() -> String {
    // NB: PostgreSQL has no min/max aggregate for `uuid`, so pick a representative node via the
    // text form — the canonical lowercase-hyphenated uuid sorts identically to the binary ordering,
    // and NULL node_ids are ignored by the aggregate (sample_node stays optional).
    let sig = signature_coalesce_sql();
    let limit = extra_bind();
    format!(
        "SELECT e.kind, {sig} AS sig, count(*) AS n, \
                min(e.node_id::text)::uuid AS sample_node \
         FROM events e LEFT JOIN nodes n ON n.id = e.node_id \
         WHERE {EVENT_FILTER_WHERE} AND e.matched_rule_id IS NULL \
           AND {sig} IS NOT NULL \
         GROUP BY e.kind, sig ORDER BY n DESC LIMIT {limit}"
    )
}

/// Authentication-failure volume by (source IP, node).
pub(super) fn agg_auth_sources_sql() -> String {
    let auth = auth_signal_predicate();
    let limit = extra_bind();
    format!(
        "SELECT host(e.source_ip) AS src, e.node_id, count(*) AS n \
         FROM events e LEFT JOIN nodes n ON n.id = e.node_id \
         WHERE {EVENT_FILTER_WHERE} AND ({auth}) \
         GROUP BY src, e.node_id ORDER BY n DESC LIMIT {limit}"
    )
}

/// Build the categorical `/events/stats` SQL for a group dimension. All identifiers are fixed
/// (chosen by the enum, never from the request); binds are the filter's plus one row cap.
pub(super) fn stats_grouped_sql(group: EventStatGroup) -> String {
    let (select, group_by, extra) = match group {
        EventStatGroup::Kind => ("e.kind AS key", "e.kind", ""),
        EventStatGroup::Action => ("e.action AS key", "e.action", ""),
        EventStatGroup::Trap => (
            "e.trap_oid AS key",
            "e.trap_oid",
            " AND e.trap_oid IS NOT NULL",
        ),
        EventStatGroup::Source => (
            "e.node_id AS node_id, host(e.source_ip) AS source_ip",
            "e.node_id, host(e.source_ip)",
            "",
        ),
    };
    let limit = extra_bind();
    format!(
        "SELECT {select}, count(*) AS n \
         FROM events e LEFT JOIN nodes n ON n.id = e.node_id \
         WHERE {EVENT_FILTER_WHERE}{extra} \
         GROUP BY {group_by} ORDER BY n DESC LIMIT {limit}"
    )
}

/// Build the time-series `/events/stats` SQL. Buckets on event time (`at_unix_ms`) into windows
/// whose width is the trailing bind; the leading binds are the filter's.
pub(super) fn stats_series_sql(split_kind: bool) -> String {
    let (select_kind, group_kind) = if split_kind {
        (", e.kind AS kind", ", e.kind")
    } else {
        ("", "")
    };
    let secs = extra_bind();
    format!(
        "SELECT (e.at_unix_ms / 1000 / {secs}) * {secs} AS bucket{select_kind}, count(*) AS n \
         FROM events e LEFT JOIN nodes n ON n.id = e.node_id \
         WHERE {EVENT_FILTER_WHERE} \
         GROUP BY bucket{group_kind} ORDER BY bucket ASC"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_list_orders_and_pages_on_the_column_the_filter_cursors_on() {
        // These three have to be the same column or paging is broken: the cursor predicate, the
        // ORDER BY, and (in `stats_series_sql`) the bucketing. They were not — the predicate and
        // ordering used `recorded_at` while the bucketing used `at_unix_ms`.
        let sql = list_events_sql();
        assert!(
            sql.contains(&format!(
                "ORDER BY e.at_unix_ms DESC LIMIT {}",
                extra_bind()
            )),
            "{sql}"
        );
        assert!(sql.contains(EVENT_FILTER_WHERE), "{sql}");
        assert!(EVENT_FILTER_WHERE.contains("e.at_unix_ms < $1"));
        assert!(
            stats_series_sql(false).contains(&format!("(e.at_unix_ms / 1000 / {})", extra_bind()))
        );
        // `recorded_at` is still selected and returned (it is real information), just never
        // filtered or ordered on.
        assert!(sql.contains("e.recorded_at,"), "{sql}");
    }

    #[test]
    fn ms_bound_converts_to_the_epoch_millis_the_predicate_compares() {
        let t = DateTime::parse_from_rfc3339("2026-07-28T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(ms_bound(Some(t)), Some(t.timestamp_millis()));
        assert_eq!(ms_bound(None), None);
    }

    /// Why `event_flap_stats` has no `LogStore` twin, stated as an invariant rather than a comment.
    ///
    /// The other four analytics aggregates were answering from the alert-linked subset whenever a
    /// log store was configured. This one was not, and the reason is that every action it counts is
    /// alert-linked — so PostgreSQL is complete for it. If a future action stops satisfying that,
    /// this fails and the twin becomes necessary.
    #[test]
    fn event_flap_only_counts_rows_postgresql_keeps() {
        // The actions the SQL counts, read off the statement itself so the two cannot drift.
        let sql = "count(*) FILTER (WHERE e.action IN ('fired','refreshed')) AS fires, \
                   count(*) FILTER (WHERE e.action = 'cleared') AS clears";
        for action in [
            EventAction::Fired,
            EventAction::Refreshed,
            EventAction::Cleared,
        ] {
            assert!(
                sql.contains(&format!("'{}'", action.as_str())),
                "{action:?} is no longer counted by event_flap_stats"
            );
            assert!(
                action.is_alert_linked(),
                "{action:?} is counted by event_flap_stats but PostgreSQL does not keep it when a \
                 log store is configured — event_flap now needs a LogStore twin"
            );
        }
        // …and the actions it does *not* count are exactly the ones PostgreSQL may drop.
        for action in [EventAction::None, EventAction::Info] {
            assert!(!action.is_alert_linked());
            assert!(!sql.contains(&format!("'{}'", action.as_str())));
        }
    }

    /// The auth vocabulary is shared with the LogsQL side, so it must be usable there: an empty
    /// phrase would match everything, and a quote would need escaping the LogsQL builder does not
    /// do for a constant.
    #[test]
    fn the_auth_signal_vocabulary_is_shared_and_usable_by_both_backends() {
        let sql = auth_signal_predicate();
        assert!(sql.contains(AUTH_FAILURE_TRAP_OID));
        for p in AUTH_FAILURE_PHRASES {
            assert!(!p.trim().is_empty(), "an empty phrase matches everything");
            assert!(
                !p.contains('"') && !p.contains('\\'),
                "unquotable phrase: {p}"
            );
            assert_eq!(p, p.to_lowercase(), "phrases are matched lowercased: {p}");
            assert!(sql.contains(p), "{p} missing from the SQL predicate");
        }
        // ILIKE, not LIKE: the SQL side is case-insensitive, matching `i(...)` on the LogsQL side.
        assert!(sql.contains("ILIKE"), "{sql}");
    }

    /// The four analytics aggregates that gained a log-store twin read through the **shared**
    /// predicate, so the window, the time basis and — the part that matters — the group scope are
    /// the same ones `/events` applies.
    ///
    /// Before this they took raw `from_ms`/`to_ms` and had no scope clause at all, which is how
    /// `rule_gap` and `auth_probe` ended up restricting after the grouping instead of before it.
    #[test]
    fn the_analytics_aggregates_share_the_event_filter_predicate() {
        for sql in [
            agg_counts_by_bucket_sql(),
            agg_severity_counts_sql(),
            agg_unmatched_signatures_sql(),
            agg_auth_sources_sql(),
        ] {
            assert!(sql.contains(EVENT_FILTER_WHERE), "{sql}");
            // `$9` is the group-scope bind. Losing it is the fail-open direction — a scoped caller
            // would silently get fleet-wide counts.
            assert!(sql.contains("$9::uuid[]"), "{sql}");
            // Event time, not `recorded_at`: the axis both backends were unified on.
            assert!(
                sql.contains("at_unix_ms") || sql.contains(EVENT_FILTER_WHERE),
                "{sql}"
            );
            assert!(sql.contains("count(*) AS n"), "{sql}");
        }
        // The two capped ones take their cap as the bind after the filter, like the stats builders.
        let cap = format!("LIMIT {}", extra_bind());
        assert!(agg_unmatched_signatures_sql().contains(&cap));
        assert!(agg_auth_sources_sql().contains(&cap));
    }

    /// The predicate declares `$1..=$N` and [`bind_event_filter`] supplies N values; nothing makes
    /// the two agree.
    ///
    /// This is the `the_events_insert_binds_every_column_it_names` failure in its other location,
    /// and it is worse here: a short bind list is a runtime error PostgreSQL reports, but a
    /// *reordered* or miscounted one runs fine and answers a different question — a page of events
    /// filtered by something the operator did not ask for, with nothing in the logs.
    /// The PostgreSQL half of `logstore::a_negated_term_stays_a_phrase_filter_too` — the message
    /// column must carry *both* spellings, with the flag choosing between them at query time
    /// (the predicate is a constant, so it cannot be chosen at build time).
    ///
    /// ⚠️ Pair it with the edge's `every_regex_parameter_accepts_a_pattern_that_compiles`, and know
    /// what neither one proves. A clause that reads correctly here still would if the edge refused
    /// every pattern, which is what it did; and neither test shows the compiled pattern is
    /// *evaluated* as a pattern rather than as a literal. The check that does is behavioural and
    /// lives on real data: a term with a metacharacter that matches nothing literally
    /// (`FIREWALL.TCK` against `FIREWALLATCK`) must still return the row.
    #[test]
    fn the_message_condition_offers_both_spellings_and_the_source_condition_only_one() {
        for needle in [
            "CASE WHEN $13::boolean THEN e.message ~* $12",
            "ELSE e.message ILIKE '%' || $12 || '%' END",
        ] {
            assert!(
                EVENT_FILTER_WHERE.contains(needle),
                "the message condition lost its {needle:?} branch"
            );
        }
        // And the source condition has no regex spelling at all — see `TextCond`: a pattern there
        // would mean "IP or name" on PostgreSQL and "IP only" on a log store.
        for absent in ["host(e.source_ip) ~*", "n.name ~*"] {
            assert!(
                !EVENT_FILTER_WHERE.contains(absent),
                "the source condition grew a regex form ({absent}); decide what it means on a log \
                 store first"
            );
        }
    }

    /// Every consumer's own trailing bind sits immediately after the filter's.
    ///
    /// Written as a derivation rather than a literal because it *was* a literal: six `$10`s in five
    /// builders, so widening the filter meant finding all six. Missing one is silent — the page
    /// size lands in a filter slot and the cap lands nowhere.
    #[test]
    fn the_extra_bind_follows_the_predicate() {
        let extra = extra_bind();
        for sql in [
            list_events_sql(),
            agg_counts_by_bucket_sql(),
            agg_unmatched_signatures_sql(),
            agg_auth_sources_sql(),
            stats_grouped_sql(EventStatGroup::Kind),
            stats_series_sql(false),
        ] {
            assert!(sql.contains(&extra), "no {extra} in: {sql}");
            // Nothing may reach past it either — a second extra bind would need a second constant.
            let beyond = format!("${}", EVENT_FILTER_BINDS + 2);
            assert!(!sql.contains(&beyond), "{beyond} in: {sql}");
        }
    }

    /// Every [`SIGNATURE_TIERS`] entry can actually be read off a record.
    ///
    /// The two lists exist separately because one names SQL/LogsQL columns and the other reaches
    /// into Rust structs, and only this pins them together. A tier added to `SIGNATURE_TIERS` and
    /// not to the accessors would leave the in-memory log store — the fake every analysis test runs
    /// against — one tier behind the two stores it stands in for, which is the failure mode where
    /// the tests pass and the deployment is wrong.
    #[test]
    fn every_signature_tier_has_an_accessor() {
        let named: Vec<&str> = SIGNATURE_TIER_ACCESSORS.iter().map(|(n, _)| *n).collect();
        assert_eq!(named, SIGNATURE_TIERS.to_vec());
    }

    #[test]
    fn stats_grouped_sql_uses_shared_filter_and_fixed_columns() {
        // Every group reuses the shared filter predicate and only fixed identifiers reach SQL.
        for g in [
            EventStatGroup::Kind,
            EventStatGroup::Action,
            EventStatGroup::Trap,
            EventStatGroup::Source,
        ] {
            let sql = stats_grouped_sql(g);
            assert!(sql.contains(EVENT_FILTER_WHERE), "{sql}");
            assert!(sql.contains("count(*) AS n"), "{sql}");
            assert!(
                sql.contains(&format!("ORDER BY n DESC LIMIT {}", extra_bind())),
                "{sql}"
            );
        }
        // Trap grouping drops NULL OIDs; source grouping carries node_id + source_ip for the UI.
        assert!(stats_grouped_sql(EventStatGroup::Trap).contains("e.trap_oid IS NOT NULL"));
        assert!(
            stats_grouped_sql(EventStatGroup::Source).contains("host(e.source_ip) AS source_ip")
        );
        assert!(stats_grouped_sql(EventStatGroup::Kind).contains("GROUP BY e.kind"));
    }

    #[test]
    fn stats_series_sql_buckets_and_optionally_splits_by_kind() {
        let plain = stats_series_sql(false);
        assert!(
            plain.contains(&format!(
                "(e.at_unix_ms / 1000 / {b}) * {b} AS bucket",
                b = extra_bind()
            )),
            "{plain}"
        );
        assert!(plain.contains(EVENT_FILTER_WHERE), "{plain}");
        assert!(
            plain.contains("GROUP BY bucket ORDER BY bucket ASC"),
            "{plain}"
        );
        assert!(!plain.contains("e.kind AS kind"), "{plain}");
        // split=kind adds the per-kind column + group key.
        let split = stats_series_sql(true);
        assert!(split.contains("e.kind AS kind"), "{split}");
        assert!(split.contains("GROUP BY bucket, e.kind"), "{split}");
    }
}
