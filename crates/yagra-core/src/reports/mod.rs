// SPDX-License-Identifier: AGPL-3.0-only
//! Reports (Dashboard → Reports).
//!
//! A **report definition** is a reusable, customizable template — a name plus an opaque `spec`
//! (the selected sections + their settings + the time range). A **schedule** runs a definition on a
//! preset cadence. A **run** is one generated report, saved for later viewing/export. Reports are a
//! SHARED resource: everyone reads, only admins write (mirrors `shared_dashboard`; the write gate is
//! at the API edge).
//!
//! Generation is a TSDB + PostgreSQL read computation — it never touches a device, so (like the
//! analysis runner, ADR-022) it runs as a background `tokio` task inside core, not a poller/bus job.
//! [`ReportRunner::run_now`] inserts a run row, spawns the task, and returns immediately; the task
//! renders each section (querying the same store/inventory/alert/history seams the rest of core
//! uses), persists the result (structured JSON + rendered HTML), and broadcasts progress over SSE.
//! Definitions/schedules/runs are metadata, so they live in PostgreSQL ([`ReportsRepo`], ADR-004).
//!
//! ## How this module is split (ADR-102)
//!
//! By **which stage of making one report** a piece belongs to, and the mechanical form of that
//! question is **"does it `.await`?"** — the two agreed exactly when it was measured, which is why
//! the rule is worth having rather than a description after the fact.
//!
//! | file | the question it answers |
//! |---|---|
//! | [`types`] | what a report *is* — the enums, the DTOs, the spec the WebUI owns |
//! | [`catalog`] | what you may put in one — the section menu `GET /reports/sections` serves |
//! | [`render`] | how it looks — a section's value, the HTML, the SVG, the CSV, the document |
//! | [`repo`] | where it is kept — definitions, schedules and runs in PostgreSQL |
//! | [`runner`] | how one gets made — insert, spawn, drive the sections, persist, broadcast |
//! | [`sections`] | where each section's numbers come from — the six `render_*` |
//! | [`seams`] | what making one is allowed to *reach* — three traits, and their live impls |
//!
//! 🚨 **`types`, `catalog` and `render` never `.await`, and `guards` refuses it.** That is not
//! tidiness: it is what keeps the pure half of a section — the arithmetic, the selector, the
//! formatting — somewhere a test can reach, which is where all seven of this module's behaviour
//! tests already live. The impure half fetches and hands off, and nothing else.
//!
//! Shared vocabulary lives **here** rather than in a sibling, for the reason `repo/mod.rs` and
//! `events/mod.rs` give: a child sees its parent's private items, so the constants, the clock and
//! the two date formatters cost no `pub(super)` at all.

use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{TimeZone, Utc};

mod catalog;
mod render;
mod repo;
mod runner;
mod seams;
mod sections;
mod types;

#[cfg(test)]
mod guards;
// The fakes a behaviour test builds a `ReportRunner` from (ADR-112). Test-only, so
// `module_source` derives it out and the source-reading checks never see it.
#[cfg(test)]
mod testkit;

pub use catalog::*;
pub use render::*;
pub use repo::*;
pub use runner::*;
pub use types::*;

/// Broadcast buffer for the run-status SSE stream (matches the analysis runner's sizing).
const EVENT_BUFFER: usize = 256;
/// Default report window when a spec omits `range_secs` (7 days).
const DEFAULT_RANGE_SECS: i64 = 7 * 86_400;
/// Target sample count for a time-series section (bounds the step so a long window stays cheap).
const MAX_POINTS: i64 = 240;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn now_s() -> i64 {
    now_ms() / 1000
}

/// Format a Unix-seconds instant as a UTC date (YYYY-MM-DD).
fn fmt_day(s: i64) -> String {
    Utc.timestamp_opt(s, 0)
        .single()
        .map_or_else(|| s.to_string(), |t| t.format("%Y-%m-%d").to_string())
}

/// Format a Unix-seconds instant as a UTC date+time (YYYY-MM-DD HH:MM UTC).
fn fmt_minute(s: i64) -> String {
    Utc.timestamp_opt(s, 0).single().map_or_else(
        || s.to_string(),
        |t| t.format("%Y-%m-%d %H:%M UTC").to_string(),
    )
}
