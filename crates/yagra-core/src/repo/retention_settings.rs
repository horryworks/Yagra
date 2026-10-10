// SPDX-License-Identifier: AGPL-3.0-only
//! The operator-editable retention windows as stored on the singleton `app_settings` row, their
//! compiled defaults and the bands they must stay inside (ADR-040).
//!
//! The policy table itself — which data each window governs and where its prune runs — is
//! `retention.rs`. These are the values that table reads, and they live with the repository that
//! reads and writes them because this layer may not reach up into the modules that use it
//! (ADR-202). The compiled constants are both the first-boot defaults and the fallback a reader
//! degrades to, so a transient database failure can never silently change the policy.

/// Alert-linked PostgreSQL data: alert history, node-state snapshots, DNS chain changes, and
/// matched passive events. One number because these are the "90-day must-preserve subset" that
/// ADR-024 contrasts with the log store's shorter search window — splitting them would let the
/// four drift apart with nothing to justify the difference.
pub const DEFAULT_ALERT_LINKED_DAYS: u32 = 90;

/// Unmatched passive events exist for rule authoring only, so they get hours rather than days.
/// Note this window is dead on a deployment with the log store enabled (ADR-024): unmatched rows
/// never reach PostgreSQL there, and the log store keeps the full firehose under its own TTL.
pub const DEFAULT_UNMATCHED_EVENT_HOURS: u32 = 24;

/// Generated report runs. Equal to [`DEFAULT_ALERT_LINKED_DAYS`] today but deliberately its own
/// name: report artefacts are regenerable and alert history is not, so lowering one must never
/// silently lower the other.
pub const DEFAULT_REPORT_RUN_DAYS: u32 = 90;

/// ClickHouse flow records and their 5-minute rollup (ADR-031, the loss-tolerant tier).
pub const DEFAULT_FLOW_DAYS: u32 = 30;

/// On-demand diagnostic artefacts: Troubleshoot analysis runs with their findings (ADR-022) and
/// generated LLM root-cause reports (ADR-029). One number over two subjects because they are the
/// same *class* — a diagnosis someone asked for, reproducible by asking again — and splitting them
/// would be two knobs nobody could tell apart. Deliberately not [`DEFAULT_REPORT_RUN_DAYS`]: that
/// control is labelled "Report runs", and a window's name must not silently govern a second kind of
/// data.
pub const DEFAULT_DIAGNOSTIC_DAYS: u32 = 90;

/// Lower bound for any day-denominated window. Zero would mean "delete on write".
pub const MIN_RETENTION_DAYS: u32 = 1;
/// Upper bound (~10 years), matching the clamp `config::parse_retention_days` already applied.
pub const MAX_RETENTION_DAYS: u32 = 3650;
/// Lower bound for the unmatched-event window.
pub const MIN_RETENTION_HOURS: u32 = 1;
/// Upper bound for the unmatched-event window (~1 year), so it cannot outlive the matched window
/// by accident.
pub const MAX_RETENTION_HOURS: u32 = 8760;

const SECS_PER_DAY: i64 = 86_400;
const SECS_PER_HOUR: i64 = 3_600;

/// Whether a day-denominated retention is inside the configurable band. Shared by the API edge and
/// the tests so the bound lives in one place (the same shape as `config::interval_in_bounds`); the
/// `CHECK` constraints on `app_settings` are the backstop, not the primary guard.
#[must_use]
pub fn days_in_bounds(days: u32) -> bool {
    (MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(&days)
}

/// Whether an hour-denominated retention is inside the configurable band.
#[must_use]
pub fn hours_in_bounds(hours: u32) -> bool {
    (MIN_RETENTION_HOURS..=MAX_RETENTION_HOURS).contains(&hours)
}

/// The operator-editable retention windows, as stored on the singleton `app_settings` row.
///
/// Read with `NodeRepo::get_retention_settings`, which degrades to [`Default`] rather than failing:
/// a database blip must not quietly widen or narrow how long data is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionSettings {
    pub alert_linked_days: u32,
    pub unmatched_event_hours: u32,
    pub report_run_days: u32,
    pub flow_days: u32,
    pub diagnostic_days: u32,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            alert_linked_days: DEFAULT_ALERT_LINKED_DAYS,
            unmatched_event_hours: DEFAULT_UNMATCHED_EVENT_HOURS,
            report_run_days: DEFAULT_REPORT_RUN_DAYS,
            flow_days: DEFAULT_FLOW_DAYS,
            diagnostic_days: DEFAULT_DIAGNOSTIC_DAYS,
        }
    }
}

impl RetentionSettings {
    /// Seconds for the alert-linked window, the unit every PostgreSQL prune method takes.
    #[must_use]
    pub fn alert_linked_secs(&self) -> i64 {
        i64::from(self.alert_linked_days) * SECS_PER_DAY
    }

    /// Seconds for the unmatched-event window.
    #[must_use]
    pub fn unmatched_event_secs(&self) -> i64 {
        i64::from(self.unmatched_event_hours) * SECS_PER_HOUR
    }

    /// Seconds for the report-run window.
    #[must_use]
    pub fn report_run_secs(&self) -> i64 {
        i64::from(self.report_run_days) * SECS_PER_DAY
    }

    /// Seconds for the diagnostic-artefact window.
    #[must_use]
    pub fn diagnostic_secs(&self) -> i64 {
        i64::from(self.diagnostic_days) * SECS_PER_DAY
    }

    /// Whether every field is inside its configurable band. The API edge rejects anything else.
    #[must_use]
    pub fn in_bounds(&self) -> bool {
        days_in_bounds(self.alert_linked_days)
            && hours_in_bounds(self.unmatched_event_hours)
            && days_in_bounds(self.report_run_days)
            && days_in_bounds(self.flow_days)
            && days_in_bounds(self.diagnostic_days)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_reject_zero_and_the_absurd() {
        assert!(!days_in_bounds(0));
        assert!(days_in_bounds(MIN_RETENTION_DAYS));
        assert!(days_in_bounds(MAX_RETENTION_DAYS));
        assert!(!days_in_bounds(MAX_RETENTION_DAYS + 1));
        assert!(!hours_in_bounds(0));
        assert!(hours_in_bounds(MIN_RETENTION_HOURS));
        assert!(hours_in_bounds(MAX_RETENTION_HOURS));
        assert!(!hours_in_bounds(MAX_RETENTION_HOURS + 1));
    }

    #[test]
    fn the_defaults_are_in_bounds_and_convert_to_the_current_windows() {
        let d = RetentionSettings::default();
        assert!(d.in_bounds());
        // These three numbers are what shipped before ADR-040 made them configurable. Changing a
        // default is a policy change, not a refactor — this is the tripwire.
        assert_eq!(d.alert_linked_secs(), 90 * 86_400);
        assert_eq!(d.unmatched_event_secs(), 86_400);
        assert_eq!(d.report_run_secs(), 90 * 86_400);
        assert_eq!(d.flow_days, 30);
        assert_eq!(d.diagnostic_secs(), 90 * 86_400);
    }

    #[test]
    fn out_of_band_settings_are_rejected_field_by_field() {
        for bad in [
            RetentionSettings {
                alert_linked_days: 0,
                ..Default::default()
            },
            RetentionSettings {
                unmatched_event_hours: 0,
                ..Default::default()
            },
            RetentionSettings {
                report_run_days: MAX_RETENTION_DAYS + 1,
                ..Default::default()
            },
            RetentionSettings {
                flow_days: 0,
                ..Default::default()
            },
            RetentionSettings {
                diagnostic_days: MAX_RETENTION_DAYS + 1,
                ..Default::default()
            },
        ] {
            assert!(!bad.in_bounds(), "{bad:?} should be out of bounds");
        }
    }
}
