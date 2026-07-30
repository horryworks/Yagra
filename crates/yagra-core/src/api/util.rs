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

/// The id of a freshly created resource — the whole body of a `201`.
///
/// Deliberately one shape for every creator. The `json!({"id": …})` literal it replaces was written
/// out per handler, which is how `{"id": …}` and `{"node_id": …}` both ended up in this API for the
/// same idea; a client then needs to know which creator it called to read the id back.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CreatedId {
    pub id: Uuid,
}

/// `?limit=` on its own — the query shape for endpoints that cap a list but do not page it.
///
/// Several of these used to borrow the alert-history query struct, which also carries a `before`
/// cursor they never read. Sharing a shape you only half-use makes it look, at the call site, like
/// the endpoint supports paging when it does not.
#[derive(Deserialize)]
pub(crate) struct ListQuery {
    pub limit: Option<i64>,
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
