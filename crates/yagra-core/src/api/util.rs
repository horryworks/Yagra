// SPDX-License-Identifier: AGPL-3.0-only
//! Small helpers shared by every domain in the API layer.
//!
//! Both of these were declared *inside* a domain's banner-delimited block in `mod.rs` while being
//! called from several others — `parse_rfc3339` from the maintenance block, `now_unix_s` from the
//! config block. That is invisible while everything is one file and a compile error the moment a
//! domain moves out, so they are hoisted here ahead of the migration rather than during it.
//!
//! Keep this module for things with no better home. Anything that is really about errors belongs in
//! [`super::error`], anything about request identity or guards in [`super::extract`].

use super::ApiError;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Edge cap on an **opaque operator-authored JSON document** — a dashboard layout, a report spec.
///
/// These are bodies the backend stores without interpreting, so nothing downstream will reject an
/// absurd one; the only defence is a size check before the DB. One constant because it is one
/// policy, not a coincidence: the reports block already wrote
/// `const MAX_REPORT_SPEC_BYTES = MAX_DASHBOARD_BYTES`, reaching into the dashboard block for it,
/// which would have become a compile error the moment either domain moved out.
pub(crate) const MAX_JSON_DOC_BYTES: usize = 262_144;

/// Validate an **opaque client-authored JSON document** — the shape the backend stores without
/// interpreting.
///
/// Two checks, and they are the only two the server can be responsible for: the body is a JSON
/// object (not an array or a scalar, which the client could not read back as a document), and it is
/// small enough not to be an attack. Each caller supplies its own typed error codes, because the
/// WebUI branches on them, and its own cap, because "how big is reasonable" is a per-domain
/// judgement — a dashboard layout and a preferences blob are not the same kind of thing.
pub(crate) fn validate_opaque_doc(
    body: &serde_json::Value,
    invalid_code: &'static str,
    too_large_code: &'static str,
    noun: &str,
    max_bytes: usize,
) -> Result<(), ApiError> {
    if !body.is_object() {
        return Err(ApiError::bad_request(
            invalid_code,
            format!("{noun} must be a JSON object"),
        ));
    }
    // `to_vec` failing means the value cannot be serialized at all; treat that as over-size rather
    // than letting it through unmeasured.
    if serde_json::to_vec(body).map_or(usize::MAX, |v| v.len()) > max_bytes {
        return Err(ApiError::payload_too_large(
            too_large_code,
            format!("{noun} exceeds {max_bytes} bytes"),
        ));
    }
    Ok(())
}

/// How many tokens one comma-separated set parameter may name.
///
/// Not a query-cost guard — that was measured and there isn't one: on 6.7M events an `in(…)` list
/// covering every stored value cost what a single value cost. It is a **request-size** guard, the
/// same reason a term is capped in chars, and it is deliberately far above the largest closed set
/// this API has (`syslog_severity`, 8 values). Do not read it as a sibling of `NAME_SEARCH_NODE_LIMIT`
/// or `STATS_SCOPE_NODE_LIMIT` in [`super::eventlog`]: those two bound how much *work* a query does,
/// and their values were chosen from that.
pub(crate) const FILTER_SET_MAX_TOKENS: usize = 32;

/// Split a comma-separated set parameter into trimmed, de-duplicated tokens, preserving order.
///
/// `Some("")` yields an empty set, which means *unfiltered* — the same as omitting the parameter.
/// That is deliberate: the WebUI clears a filter by writing an empty value before it removes the
/// key, and a client that sends `kind=` should get the unfiltered list rather than a 400 or,
/// worse, a set containing one empty string that matches nothing.
pub(crate) fn split_set(raw: Option<&str>) -> Vec<String> {
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
///
/// **This lives here rather than in one domain because the spelling is the contract.** It was
/// `eventlog`'s private helper while events were the only multi-value surface; when alert history,
/// audit, thresholds and findings each gained set parameters (ADR-053 Inc.4b), a second copy would
/// have been four chances to differ from Events on the empty-string case, the cap, or whether an
/// unknown token is rejected or dropped — and the *dropped* spelling is the one that fails silently.
/// `a, b or c` — the human half of a rejection message, built from the enum that defines the set.
///
/// Beside [`parse_set`] for the reason that helper is here: the spelling is the contract. Both
/// edges hand the same sentence to a caller who names a value outside the set, and a hand-written
/// one rots the moment the enum grows — `list_thresholds` said "global, profile, group or node"
/// for as long as `ScopeLevel` had six variants, so an operator asking for a folder-group or
/// port rule was told, wrongly, that no such level existed.
pub(crate) fn token_list<'a>(tokens: impl Iterator<Item = &'a str>) -> String {
    let all: Vec<&'a str> = tokens.collect();
    match all.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_owned(),
        Some((last, head)) => format!("{} or {last}", head.join(", ")),
    }
}

pub(crate) fn parse_set<T>(
    field: &str,
    raw: Option<&str>,
    allowed: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<Vec<T>, ApiError> {
    parse_set_with(
        "invalid_filter",
        field,
        raw,
        |_| format!("{field} must be {allowed}"),
        parse,
    )
}

/// [`parse_set`] with the rejection's error code and message chosen by the caller.
///
/// For the endpoints whose single-value predecessors answered a *specific* code the WebUI branches
/// on — `invalid_tool`, `invalid_severity` on the findings search. Widening those to the shared
/// `invalid_filter` while adding multi-value support would have been a silent contract change on a
/// surface that already had a client reading the code.
pub(crate) fn parse_set_with<T>(
    code: &'static str,
    field: &str,
    raw: Option<&str>,
    message: impl Fn(&str) -> String,
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
        .map(|t| parse(t).ok_or_else(|| ApiError::bad_request(code, message(t))))
        .collect()
}

/// The id of a freshly created resource — the whole body of a `201`.
///
/// Deliberately one shape for every creator. The `json!({"id": …})` literal it replaces was written
/// out per handler, which is how `{"id": …}` and `{"node_id": …}` both ended up in this API for the
/// same idea; a client then needs to know which creator it called to read the id back.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct CreatedId {
    pub id: Uuid,
}

// ⚠️ This type derives `ToSchema`, so its **doc comments are published verbatim to API clients** in
// the generated OpenAPI document. Keep them outward-facing; the design rationale goes in plain `//`
// comments like this one, which utoipa does not read. (See a7e0180 — internal notes shipped once.)
//
// Why an envelope rather than an added field: these endpoints returned a bare array, and there is
// nowhere on an array to put a flag. Bare arrays were the wrong shape anyway — an endpoint that may
// need to say something *about* its result has no room to grow.
//
// Why the flag exists at all: rankings come from stores that know nothing about folder groups (the
// TSDB ranks by value, PostgreSQL by fire count), so a group-scoped caller's list is built by
// over-fetching `N × RANKING_OVERFETCH` and dropping what they may not see (`api/scope.rs`). That
// usually still fills N; when it does not, the short list is indistinguishable from "that is all
// there is". `partial` is the difference between an answer and a wrong answer.
//
// `partial` is always `false` for an unrestricted caller — which is every caller until a scope can
// be issued — so today's deployments pay one boolean and nothing else.
/// A ranked Top-N result.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct Ranked<T> {
    /// The ranked rows, highest first, at most the requested `limit`.
    pub entries: Vec<T>,
    /// `true` when this ranking covers only the groups the calling account may see and entries it
    /// is entitled to may be missing. Always `false` for an account with unrestricted visibility.
    pub partial: bool,
}

impl<T> Ranked<T> {
    /// Build the envelope from an already-scope-filtered ranking.
    ///
    /// `partial` is derived here rather than at each call site so the rule cannot be spelled four
    /// slightly different ways: the list is partial exactly when the filter left fewer rows than
    /// asked for **and** the store had been asked for more than that. Deriving it from `truncate`'s
    /// inputs is what makes it impossible to report `false` after silently dropping rows.
    pub fn new(mut entries: Vec<T>, limit: usize, fetched: usize) -> Self {
        let partial = entries.len() < limit && fetched > limit;
        entries.truncate(limit);
        Self { entries, partial }
    }
}

// `ListQuery` — `?limit=` on its own — lived here and is gone. It had three users (thresholds,
// analysis runs, report runs) and all three grew filters, so each now declares its own query
// struct. The lesson it was written to record still holds and is why nothing should re-create it:
// several of these once borrowed the alert-history query struct, which also carries a `before`
// cursor they never read, and sharing a shape you only half-use makes it look at the call site
// like the endpoint supports paging when it does not. A one-field query is cheap to declare where
// it is used; a shared one becomes a place every domain has to widen together.

/// The cadence fields of any create/update body that schedules something.
///
/// Two domains post these — report schedules and analysis schedules — so the fields, their
/// validation and their clamps are declared once. Flattened into each domain's own body rather than
/// nested, so the wire shape is unchanged from when reports owned it alone.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct CadenceBody {
    /// `daily` | `weekly` | `monthly`.
    pub frequency: String,
    /// 0=Sun … 6=Sat. Read only for `weekly`.
    pub day_of_week: Option<i16>,
    /// 1 … 28. Read only for `monthly`.
    pub day_of_month: Option<i16>,
    /// Hour of day to fire, UTC.
    pub at_hour: i16,
    /// Minute of the hour to fire.
    pub at_minute: i16,
}

/// Validate a cadence body into a [`crate::cadence::Schedule`].
///
/// The frequency is **rejected**, the day/hour/minute fields are **clamped**. That split is
/// deliberate: a `day_of_month` of 31 is a reasonable thing for a form to send and there is a
/// defensible answer (28, so it fires every month including February), whereas an unknown frequency
/// has no defensible answer at all.
///
/// `Unknown` is a *storage* degradation — what a row written by a newer core reads back as — and
/// never something an operator may pick. Accepting it here would let a client write a cadence the
/// scheduler then silently treats as daily.
pub(crate) fn parse_cadence(body: CadenceBody) -> Result<crate::cadence::Schedule, ApiError> {
    let frequency = crate::cadence::Cadence::from_stored(&body.frequency);
    if frequency == crate::cadence::Cadence::Unknown {
        return Err(ApiError::bad_request(
            "invalid_frequency",
            "frequency must be daily|weekly|monthly",
        ));
    }
    Ok(crate::cadence::Schedule {
        frequency,
        // Each day field is kept only for the cadence that reads it, so a weekly schedule cannot
        // carry a stale day-of-month from an edit that changed the frequency.
        day_of_week: (frequency == crate::cadence::Cadence::Weekly)
            .then(|| body.day_of_week.unwrap_or(0).clamp(0, 6)),
        day_of_month: (frequency == crate::cadence::Cadence::Monthly)
            .then(|| body.day_of_month.unwrap_or(1).clamp(1, 28)),
        at_hour: body.at_hour.clamp(0, 23),
        at_minute: body.at_minute.clamp(0, 59),
    })
}

/// Append one audit row, logging rather than failing if the write does not land.
///
/// A best-effort record on purpose: refusing a login because the audit table is unreachable would
/// turn a logging fault into an outage. The failure is logged, so it is visible.
///
/// Here rather than beside the handlers because both authentication paths write these — password
/// login and the OIDC callback — plus the audit middleware itself. `audit_mw` covers mutating
/// `/api/v1` requests generically, but the auth endpoints are excluded from it and record their own
/// rows, with the attempted **username only and never the credential** (security.md).
pub(crate) async fn audit_record(
    audit: &crate::audit::AuditRepo,
    username: &str,
    action: &str,
    status: u16,
) {
    if let Err(e) = audit.record(username, action, status).await {
        tracing::warn!(error = %e, %action, "audit record failed");
    }
}

/// The body of an enable/disable toggle — `{"enabled": true}`.
///
/// Shared rather than per-domain because it already was: the maintenance module reached across for
/// `super::EnabledBody` while it was declared inside the notifications block, which is the same
/// cross-domain reach that keeps turning up as a migration tripwire.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct EnabledBody {
    pub enabled: bool,
}

/// Build a [`PoolResolver`](crate::poolres::PoolResolver) from the folder tree.
///
/// A read error degrades to "no folder inheritance" with a warning, deliberately: every caller is a
/// read-only view, where missing inheritance is a display inaccuracy rather than a polling decision.
/// The scheduler does **not** use this — it builds its own resolver and holds the last known one, so
/// a folder-read blip can never misroute an actual poll.
///
/// Shared by the nodes and poller-pool domains, so it lives here rather than in either.
pub(crate) async fn pool_resolver(admin: &super::AdminState) -> crate::poolres::PoolResolver {
    match admin.groups.pool_rows().await {
        Ok(rows) => crate::poolres::PoolResolver::build(rows),
        Err(e) => {
            tracing::warn!(error = %e, "loading folder pools failed; resolving without inheritance");
            crate::poolres::PoolResolver::empty()
        }
    }
}

/// Rate window for interface utilization, in seconds.
///
/// Matches the TSDB query-time rate() derivation (ADR-012); five minutes covers a few poll
/// intervals so one missed poll does not blank the rate. Shared because the metrics domain and the
/// interface list must ask the same question of the same counters — two windows would make the
/// node Overview and the Interfaces tab disagree about the same link.
pub(crate) const DEFAULT_RATE_LOOKBACK_SECS: u64 = 300;

/// Whether `oid` is a dotted numeric OID (`1.3.6.1.2.1.1.1.0`).
///
/// Structural only — it does not claim the OID exists. The point is that an operator-supplied OID
/// reaches SNMP and the collection tables as a string, so anything that is not digits-and-dots is
/// rejected at the edge rather than carried inward (security.md).
///
/// Hoisted here because the MIB catalog validated with it while it was declared inside the
/// collection-sets block — the same cross-domain reach that made `not_found` a migration tripwire.
pub(crate) fn is_valid_oid(oid: &str) -> bool {
    !oid.is_empty()
        && oid
            .split('.')
            .all(|arc| !arc.is_empty() && arc.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `s` is a valid OID *prefix* — an OID, optionally written with a trailing dot
/// (`1.3.6.1.4.1.9.` reads as "everything under Cisco").
///
/// Defined in terms of [`is_valid_oid`] rather than beside it: the two were separate copies of the
/// same digits-and-dots loop, differing only in that one tolerated the trailing dot.
pub(crate) fn is_valid_oid_prefix(s: &str) -> bool {
    is_valid_oid(s.strip_suffix('.').unwrap_or(s))
}

/// Cap on a free-text search term. Long enough for any real query, short enough that a
/// pathological input cannot bloat the query sent to any store.
///
/// Here rather than in the event-log domain, which declared it while the audit log needed the same
/// bound — the cross-domain reach that keeps turning up as a migration tripwire. It is one policy,
/// not a coincidence: both terms are operator text that becomes an `ILIKE` pattern.
pub(crate) const SEARCH_TERM_MAX_CHARS: usize = 200;

/// Normalize a free-text filter: trim, drop if empty, and cap the length.
///
/// The blank-is-absent rule matters at the edge: a UI that clears its search box sends `q=`, and a
/// filter nobody set must not become a filter matching everything-or-nothing depending on the store.
pub(crate) fn normalize_search(q: Option<&str>) -> Option<String> {
    q.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(SEARCH_TERM_MAX_CHARS).collect())
}

/// Parse an RFC 3339 timestamp from the API edge into UTC.
///
/// `None` on anything unparseable: callers reject the request rather than substituting a default,
/// so a malformed bound can never widen a query silently (security.md — parse at the edge).
pub(crate) fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// The current Unix time in whole seconds.
///
/// Saturating rather than fallible: a clock before the epoch yields `0` and one past `i64::MAX`
/// yields `i64::MAX`, because every caller uses this for a stamp on a row it is about to write and
/// none of them has a sensible failure branch.
pub(crate) fn now_unix_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unrestricted_ranking_is_never_reported_as_partial() {
        // The case every deployment is in today. `ranking_fetch_limit` returns the plain limit for
        // an unrestricted caller, so `fetched == limit` and nothing was dropped — a `true` here
        // would put a "may be incomplete" warning on every dashboard widget in the product.
        let r = Ranked::new(vec![1, 2, 3], 5, 5);
        assert_eq!(r.entries, vec![1, 2, 3]);
        assert!(
            !r.partial,
            "a short list from a store that simply had less data is complete, not truncated"
        );
        // A full list is likewise complete.
        assert!(!Ranked::new(vec![1, 2, 3, 4, 5], 5, 5).partial);
    }

    #[test]
    fn a_scope_filter_that_eats_the_over_fetch_reports_partial() {
        // The case the flag exists for: 50 rows were asked for to fill a list of 5, the scope
        // filter left 2, and 2 is indistinguishable from "this caller has only 2 nodes" unless the
        // response says otherwise.
        let r = Ranked::new(vec![1, 2], 5, 50);
        assert!(r.partial);
        // …but if the over-fetch still filled the list, nothing was lost.
        let r = Ranked::new(vec![1, 2, 3, 4, 5, 6, 7], 5, 50);
        assert!(!r.partial);
        assert_eq!(r.entries, vec![1, 2, 3, 4, 5], "and it is still truncated");
    }

    #[test]
    fn truncation_and_the_flag_are_decided_together() {
        // They are one function precisely so they cannot disagree. Deciding `partial` before
        // truncating (or at a different call site) is how a list gets shortened while still
        // claiming to be complete.
        let r = Ranked::new((0..100).collect::<Vec<_>>(), 3, 30);
        assert_eq!(r.entries.len(), 3);
        assert!(!r.partial);
        let empty = Ranked::new(Vec::<u8>::new(), 3, 30);
        assert!(empty.partial, "nothing survived the filter — say so");
    }

    #[test]
    fn parses_offsets_into_utc_and_rejects_anything_else() {
        // The offset must be applied, not dropped: 09:00+09:00 is midnight UTC, and a filter that
        // read it as 09:00 UTC would silently shift a whole query window by the caller's timezone.
        let t = parse_rfc3339("2026-07-28T09:00:00+09:00").expect("a valid offset timestamp");
        assert_eq!(t.to_rfc3339(), "2026-07-28T00:00:00+00:00");

        for bad in [
            "2026-07-28",           // date only — no time, no offset
            "2026-07-28 09:00:00",  // space separator, not RFC 3339
            "2026-07-28T09:00:00",  // naive: no offset means no instant
            "yesterday",            // free text
            "",                     // empty
            "2026-13-01T00:00:00Z", // month 13
        ] {
            assert!(parse_rfc3339(bad).is_none(), "{bad} must not parse");
        }
    }

    #[test]
    fn now_is_a_plausible_present_day_epoch_second() {
        // Pins the unit (seconds, not millis) — the stamps this writes are compared against
        // `at_unix_ms` columns elsewhere, and a unit slip there is a 1000× wrong timestamp.
        let now = now_unix_s();
        assert!(now > 1_700_000_000, "{now} is before 2023");
        assert!(now < 4_000_000_000, "{now} is after 2096");
    }
}
