// SPDX-License-Identifier: AGPL-3.0-only
//! **What a report is** — the closed sets, the rows the API serves, and the document the WebUI owns.
//!
//! Nothing here reaches outside the process, which \`super::guards\` checks: no \`.await\`, no
//! \`async fn\`. The one behaviour test is the token/serde round trip the three enums owe
//! (\`testing.md\`), and it can be a plain unit test precisely because of that.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cadence::Cadence;
use crate::stored_enum::token_enum;

// ── Closed sets ───────────────────────────────────────────────────────────────────────
//
// These four were `String` on the wire, so the generated contract said "string" and the WebUI
// could not be made to handle every case. It didn't: the run-status badge ended in a `default:`
// arm painting anything unrecognised as a red "Failed", which is the worst possible reading of a
// state we simply hadn't taught it about. Naming the sets puts them in the OpenAPI document as
// enums, which makes the TypeScript a union, which lets the badge be an exhaustive map.
//
// Each carries an `Unknown` variant on purpose. A token this build does not recognise means the
// row was written by a newer core, and the alternative — mapping it onto a real state — would have
// the API assert something the database never said.

/// Lifecycle of one report run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportRunState {
    /// Accepted but not started. The current backend never writes this — [`Repo::insert_run`]
    /// inserts `running` directly — but [`Repo::fail_orphans`] sweeps it, so it stays named.
    Queued,
    /// Generating; `pct` is meaningful.
    Running,
    /// Finished; the rendered result is available.
    Succeeded,
    /// Gave up; `error` says why.
    Failed,
    /// A state this build does not know — a newer core wrote it.
    Unknown,
}

/// What started a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportRunTrigger {
    /// An operator pressed Generate.
    Manual,
    /// A schedule fired it.
    Scheduled,
    /// A trigger this build does not know — a newer core wrote it.
    Unknown,
}

/// Outcome of a schedule's most recent firing.
//  kebab-case, not snake: `missing-definition` is the token already in the column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReportScheduleStatus {
    /// A run was queued.
    Queued,
    /// The schedule outlived the definition it pointed at.
    MissingDefinition,
    /// Queuing the run failed.
    Error,
    /// A status this build does not know — a newer core wrote it.
    Unknown,
}

token_enum!(ReportRunState, Unknown, "report_runs.state", [
    Queued => "queued",
    Running => "running",
    Succeeded => "succeeded",
    Failed => "failed",
    Unknown => "unknown",
]);
token_enum!(ReportRunTrigger, Unknown, "report_runs.trigger", [
    Manual => "manual",
    Scheduled => "scheduled",
    Unknown => "unknown",
]);
token_enum!(ReportScheduleStatus, Unknown, "report_schedules.last_status", [
    Queued => "queued",
    MissingDefinition => "missing-definition",
    Error => "error",
    Unknown => "unknown",
]);

// ── Persisted shapes ──────────────────────────────────────────────────────────────────

/// A report definition (reusable template), as served to the API.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ReportDefinition {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub spec: serde_json::Value,
    pub updated_by: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// A schedule row (joined with its definition's name for display).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ReportSchedule {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub definition_name: String,
    pub frequency: Cadence,
    pub day_of_week: Option<i16>,
    pub day_of_month: Option<i16>,
    pub at_hour: i16,
    pub at_minute: i16,
    pub enabled: bool,
    pub next_run_ms: i64,
    pub last_run_ms: Option<i64>,
    pub last_status: Option<ReportScheduleStatus>,
}

/// A run row for the saved-reports list (without the heavy `result_*` payloads).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ReportRun {
    pub id: Uuid,
    pub definition_id: Option<Uuid>,
    pub name: String,
    pub trigger: ReportRunTrigger,
    pub state: ReportRunState,
    pub pct: i32,
    pub error: Option<String>,
    pub range_from_ms: Option<i64>,
    pub range_to_ms: Option<i64>,
    pub section_count: i32,
    pub created_by: Option<String>,
    pub created_ms: i64,
    pub started_ms: Option<i64>,
    pub finished_ms: Option<i64>,
}

/// A run plus its rendered result (the viewer / export endpoints).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ReportRunDetail {
    #[serde(flatten)]
    pub run: ReportRun,
    pub result_json: Option<serde_json::Value>,
    pub result_html: Option<String>,
}

// ── Spec parsing (the opaque definition document, parsed at generation time) ─────────────

/// The report document the WebUI owns. Parsed leniently (unknown fields/sections tolerated) so a
/// newer WebUI shape stays compatible with an older core (ADR-017).
#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct ReportSpec {
    #[serde(default)]
    pub(super) params: ReportParams,
    #[serde(default)]
    pub(super) sections: Vec<SectionSpec>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct ReportParams {
    #[serde(default)]
    pub(super) range_secs: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct SectionSpec {
    #[serde(default)]
    pub(super) id: Option<String>,
    pub(super) kind: String,
    #[serde(default)]
    pub(super) settings: serde_json::Value,
}

/// Read a numeric setting (accepts JSON number or numeric string), clamped by the caller.
pub(super) fn setting_i64(settings: &serde_json::Value, key: &str, default: i64) -> i64 {
    match settings.get(key) {
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(default),
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(default),
        _ => default,
    }
}

/// Read a string setting.
pub(super) fn setting_str(settings: &serde_json::Value, key: &str, default: &str) -> String {
    settings
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(default)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The token an enum writes to its column and the tag serde puts on the wire are the same
    /// string produced two different ways (`as_str` vs `rename_all`). Nothing makes them agree, and
    /// a disagreement would mean rows this build writes are rows it cannot read back.
    #[test]
    fn token_and_serde_agree_for_every_report_enum() {
        macro_rules! check {
            ($t:ty) => {
                for v in <$t>::ALL.iter().copied() {
                    assert_eq!(
                        serde_json::to_string(&v).unwrap(),
                        format!("\"{}\"", v.as_str()),
                        "{:?} serializes differently from its token",
                        v
                    );
                    assert_eq!(<$t>::from_stored(v.as_str()), v);
                }
            };
        }
        check!(ReportRunState);
        check!(ReportRunTrigger);
        check!(ReportScheduleStatus);
        // `Cadence` is checked the same way in `cadence.rs` — it is shared with analysis schedules
        // now, so its test belongs beside it rather than in whichever feature happens to store it.

        // The kebab-case outlier, spelled out so nobody "tidies" it to snake_case: this token is
        // already in the column.
        assert_eq!(
            ReportScheduleStatus::MissingDefinition.as_str(),
            "missing-definition"
        );
        // An unrecognised token degrades rather than failing the read.
        assert_eq!(
            ReportRunState::from_stored("cancelled"),
            ReportRunState::Unknown
        );
    }
}
