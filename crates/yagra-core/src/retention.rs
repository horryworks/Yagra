//! Data retention policy (ADR-040) — the one place that answers "how long does Yagra keep X".
//!
//! Before this module the answer was spread over eight sites and the number `90 * 86_400` was
//! written three times, with no file anywhere stating the policy. The rule now is: **this module
//! declares the table, and every prune site implements it**. A new retained table adds a
//! [`Subject`] variant, which forces a row in [`Subject::row`] because that match is exhaustive.
//!
//! Retention is not uniformly ours to set. Where the rule *lives* decides whether an operator can
//! change it at runtime, and the three cases are genuinely different:
//!
//! * **PostgreSQL** — the rule is our own `DELETE … WHERE ts < …`, so it is freely configurable.
//! * **ClickHouse** — the rule is table DDL (`TTL ts + INTERVAL n DAY`), reachable over the SQL
//!   connection we already hold, so it is configurable via `ALTER TABLE … MODIFY TTL`.
//! * **VictoriaMetrics / VictoriaLogs** — the rule is another process's *start flag*
//!   (`--retentionPeriod`), and those products expose no runtime API to change it. Reaching it
//!   would mean rewriting a compose file on the host and restarting a sibling container, i.e.
//!   mounting the Docker socket into core — host-root in exchange for one knob. Not worth it.
//!   Those two rows are therefore read-only, and the UI shows the value it read back *from the
//!   store itself* (`GET /flags`) rather than mirroring a number into an env var that could
//!   silently disagree with what the store is actually enforcing.
//!
//! The compiled constants below are both the first-boot defaults and the fallback a reader
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
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            alert_linked_days: DEFAULT_ALERT_LINKED_DAYS,
            unmatched_event_hours: DEFAULT_UNMATCHED_EVENT_HOURS,
            report_run_days: DEFAULT_REPORT_RUN_DAYS,
            flow_days: DEFAULT_FLOW_DAYS,
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

    /// Whether every field is inside its configurable band. The API edge rejects anything else.
    #[must_use]
    pub fn in_bounds(&self) -> bool {
        days_in_bounds(self.alert_linked_days)
            && hours_in_bounds(self.unmatched_event_hours)
            && days_in_bounds(self.report_run_days)
            && days_in_bounds(self.flow_days)
    }
}

/// The flag name both VictoriaMetrics and VictoriaLogs use for their retention window.
const RETENTION_FLAG: &str = "-retentionPeriod=";

/// Pull the retention window out of a VictoriaMetrics/VictoriaLogs `GET /flags` body.
///
/// Those two products keep retention in a process start flag with no runtime API to change it, so
/// the honest thing to show an operator is what the store is *actually* enforcing rather than a
/// number mirrored into an env var that can silently disagree with the compose file. Both expose
/// `/flags`, one `-name="value"` per line, e.g.:
///
/// ```text
/// -retentionPeriod="30d"
/// -storageDataPath="/victoria-logs-data"
/// ```
///
/// `/flags` lists only flags that were **explicitly set**, so `None` here means "running the
/// product's own default" — a real answer, and deliberately not the same as guessing a number.
#[must_use]
pub fn parse_retention_flag(body: &str) -> Option<String> {
    body.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(RETENTION_FLAG))
        .map(|v| v.trim().trim_matches('"').trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Everything Yagra retains on a schedule. Exhaustive by construction: [`Subject::row`] matches on
/// every variant, so a new retained table cannot ship without declaring its policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    AlertHistory,
    NodeStateSnapshots,
    DnsChainChanges,
    EventsMatched,
    EventsUnmatched,
    ReportRuns,
    FlowRecords,
    EventLogStore,
    Metrics,
    AuditLog,
}

/// How the retention is actually enforced — which decides who *can* change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    /// A `DELETE` statement Yagra runs on a timer.
    PgPrune,
    /// A TTL declared in the store's own schema, which Yagra can alter.
    StoreTtl,
    /// A command-line flag of a store process Yagra does not control.
    StoreFlag,
    /// Kept forever unless an operator deletes it deliberately.
    Unlimited,
}

/// Where an operator changes this row, if anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tunable {
    /// Editable from Settings ▸ System settings.
    Settings,
    /// Set on the store container's command line; shown read-only with the knob named.
    StoreFlagReadOnly,
    /// Not retained on a schedule at all, by decision of this ADR.
    ByDecision,
}

/// Which [`RetentionSettings`] field backs a row. `StoreOwned` and `Unlimited` are the two
/// non-fields, and the biconditional against [`Tunable`] is tested — a row that renders as editable
/// but is bound to no field would be a control that silently does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    AlertLinkedDays,
    UnmatchedEventHours,
    ReportRunDays,
    FlowDays,
    StoreOwned,
    Unlimited,
}

impl Enforcement {
    /// Stable token for the API/UI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Enforcement::PgPrune => "pg_prune",
            Enforcement::StoreTtl => "store_ttl",
            Enforcement::StoreFlag => "store_flag",
            Enforcement::Unlimited => "unlimited",
        }
    }
}

impl Tunable {
    /// Stable token for the API/UI. The UI keys its "is this row editable" decision off this, so it
    /// is a `Record`-style exhaustive lookup on the TypeScript side too.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Tunable::Settings => "settings",
            Tunable::StoreFlagReadOnly => "store_flag_read_only",
            Tunable::ByDecision => "by_decision",
        }
    }
}

impl Field {
    /// Stable token for the API/UI — this is the name of the form control a row binds to.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Field::AlertLinkedDays => "alert_linked_days",
            Field::UnmatchedEventHours => "unmatched_event_hours",
            Field::ReportRunDays => "report_run_days",
            Field::FlowDays => "flow_days",
            Field::StoreOwned => "store_owned",
            Field::Unlimited => "unlimited",
        }
    }
}

/// One line of the retention table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub subject: Subject,
    /// Which store holds the data.
    pub store: &'static str,
    pub enforcement: Enforcement,
    pub tunable: Tunable,
    pub field: Field,
    /// Operator-facing note. For a read-only row this names the knob that does change it.
    pub note: &'static str,
}

impl Subject {
    /// Every subject, in the order the UI and the ADR table present them.
    pub const ALL: [Subject; 10] = [
        Subject::AlertHistory,
        Subject::NodeStateSnapshots,
        Subject::DnsChainChanges,
        Subject::EventsMatched,
        Subject::EventsUnmatched,
        Subject::ReportRuns,
        Subject::FlowRecords,
        Subject::EventLogStore,
        Subject::Metrics,
        Subject::AuditLog,
    ];

    /// A stable token for this subject, used as the API/UI key. Kept `snake_case` to match the
    /// serde convention used across the API surface.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Subject::AlertHistory => "alert_history",
            Subject::NodeStateSnapshots => "node_state_snapshots",
            Subject::DnsChainChanges => "dns_chain_changes",
            Subject::EventsMatched => "events_matched",
            Subject::EventsUnmatched => "events_unmatched",
            Subject::ReportRuns => "report_runs",
            Subject::FlowRecords => "flow_records",
            Subject::EventLogStore => "event_log_store",
            Subject::Metrics => "metrics",
            Subject::AuditLog => "audit_log",
        }
    }

    /// This subject's policy row. Exhaustive on purpose — see the module doc.
    #[must_use]
    pub const fn row(self) -> Row {
        match self {
            Subject::AlertHistory => Row {
                subject: self,
                store: "PostgreSQL",
                enforcement: Enforcement::PgPrune,
                tunable: Tunable::Settings,
                field: Field::AlertLinkedDays,
                note: "Fired/cleared alert records, including the metric snapshot taken at fire time.",
            },
            Subject::NodeStateSnapshots => Row {
                subject: self,
                store: "PostgreSQL",
                enforcement: Enforcement::PgPrune,
                tunable: Tunable::Settings,
                field: Field::AlertLinkedDays,
                note: "Five-minute fleet state counts behind the dashboard's degrading/recovering chart.",
            },
            Subject::DnsChainChanges => Row {
                subject: self,
                store: "PostgreSQL",
                enforcement: Enforcement::PgPrune,
                tunable: Tunable::Settings,
                field: Field::AlertLinkedDays,
                note: "Append-on-change DNS resolution chains; a healthy fleet writes almost nothing here.",
            },
            Subject::EventsMatched => Row {
                subject: self,
                store: "PostgreSQL",
                enforcement: Enforcement::PgPrune,
                tunable: Tunable::Settings,
                field: Field::AlertLinkedDays,
                note: "Passive events that matched a rule, so they are linked to alert history.",
            },
            Subject::EventsUnmatched => Row {
                subject: self,
                store: "PostgreSQL",
                enforcement: Enforcement::PgPrune,
                tunable: Tunable::Settings,
                field: Field::UnmatchedEventHours,
                note: "Passive events that matched no rule, kept as rule-authoring material. Not written at all when a log store is configured.",
            },
            Subject::ReportRuns => Row {
                subject: self,
                store: "PostgreSQL",
                enforcement: Enforcement::PgPrune,
                tunable: Tunable::Settings,
                field: Field::ReportRunDays,
                note: "Generated report artefacts. Regenerable from their definition.",
            },
            Subject::FlowRecords => Row {
                subject: self,
                store: "ClickHouse",
                enforcement: Enforcement::StoreTtl,
                tunable: Tunable::Settings,
                field: Field::FlowDays,
                note: "Traffic-flow records and their 5-minute rollup. Applied as a table TTL; lowering it deletes existing rows.",
            },
            Subject::EventLogStore => Row {
                subject: self,
                store: "VictoriaLogs",
                enforcement: Enforcement::StoreFlag,
                tunable: Tunable::StoreFlagReadOnly,
                field: Field::StoreOwned,
                note: "Set by the victorialogs container's -retentionPeriod flag; change it in the compose file and recreate that container.",
            },
            Subject::Metrics => Row {
                subject: self,
                store: "VictoriaMetrics",
                enforcement: Enforcement::StoreFlag,
                tunable: Tunable::StoreFlagReadOnly,
                field: Field::StoreOwned,
                note: "Set by the victoriametrics container's --retentionPeriod flag; change it in the compose file and recreate that container.",
            },
            Subject::AuditLog => Row {
                subject: self,
                store: "PostgreSQL",
                enforcement: Enforcement::Unlimited,
                tunable: Tunable::ByDecision,
                field: Field::Unlimited,
                note: "Kept indefinitely by decision (ADR-040): who changed what must not be swept away as a side effect of tidying logs.",
            },
        }
    }

    /// The currently configured window for this subject, or `None` when the store owns it or it is
    /// unlimited. Days for every field except [`Field::UnmatchedEventHours`], which is hours —
    /// callers pair this with [`Row::unit`].
    #[must_use]
    pub fn configured(self, s: &RetentionSettings) -> Option<u32> {
        match self.row().field {
            Field::AlertLinkedDays => Some(s.alert_linked_days),
            Field::UnmatchedEventHours => Some(s.unmatched_event_hours),
            Field::ReportRunDays => Some(s.report_run_days),
            Field::FlowDays => Some(s.flow_days),
            Field::StoreOwned | Field::Unlimited => None,
        }
    }
}

impl Row {
    /// The unit `configured` is denominated in, for display.
    #[must_use]
    pub const fn unit(&self) -> &'static str {
        match self.field {
            Field::UnmatchedEventHours => "hours",
            Field::AlertLinkedDays | Field::ReportRunDays | Field::FlowDays => "days",
            Field::StoreOwned | Field::Unlimited => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_subject_has_a_row() {
        // Binding each variant by name is what makes this fail to compile — not merely fail — when
        // a subject is added without being listed in ALL.
        for s in Subject::ALL {
            let row = s.row();
            assert_eq!(row.subject, s, "row() returned another subject's row");
            assert!(!row.store.is_empty());
            assert!(!row.note.is_empty(), "{} has no operator note", s.as_str());
        }
        let counted = Subject::ALL.len();
        let named = [
            Subject::AlertHistory,
            Subject::NodeStateSnapshots,
            Subject::DnsChainChanges,
            Subject::EventsMatched,
            Subject::EventsUnmatched,
            Subject::ReportRuns,
            Subject::FlowRecords,
            Subject::EventLogStore,
            Subject::Metrics,
            Subject::AuditLog,
        ];
        assert_eq!(counted, named.len(), "ALL is missing a subject");
    }

    #[test]
    fn subject_tokens_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for s in Subject::ALL {
            assert!(seen.insert(s.as_str()), "duplicate token {}", s.as_str());
        }
    }

    /// The dangerous shape this guards: a row that renders as editable but is bound to no settings
    /// field would be a control an operator can move that changes nothing.
    #[test]
    fn tunability_and_field_agree_in_both_directions() {
        for s in Subject::ALL {
            let row = s.row();
            let is_settings_field = matches!(
                row.field,
                Field::AlertLinkedDays
                    | Field::UnmatchedEventHours
                    | Field::ReportRunDays
                    | Field::FlowDays
            );
            match row.tunable {
                Tunable::Settings => assert!(
                    is_settings_field,
                    "{} is editable but backed by no settings field",
                    s.as_str()
                ),
                Tunable::StoreFlagReadOnly => assert_eq!(
                    row.field,
                    Field::StoreOwned,
                    "{} is store-owned but claims a settings field",
                    s.as_str()
                ),
                Tunable::ByDecision => assert_eq!(
                    row.field,
                    Field::Unlimited,
                    "{} is unlimited but claims a settings field",
                    s.as_str()
                ),
            }
            assert_eq!(
                is_settings_field,
                s.configured(&RetentionSettings::default()).is_some(),
                "{}: `configured` disagrees with the field",
                s.as_str()
            );
        }
    }

    /// ADR-040 decision (2): only a store flag is read-only, and only the audit log is unlimited.
    /// Both directions, so a new row cannot quietly join either group.
    #[test]
    fn enforcement_and_tunability_agree() {
        for s in Subject::ALL {
            let row = s.row();
            assert_eq!(
                row.enforcement == Enforcement::StoreFlag,
                row.tunable == Tunable::StoreFlagReadOnly,
                "{}: read-only iff the store owns the flag",
                s.as_str()
            );
            assert_eq!(
                row.enforcement == Enforcement::Unlimited,
                s == Subject::AuditLog,
                "{}: the audit log is the only unlimited subject",
                s.as_str()
            );
        }
    }

    #[test]
    fn a_read_only_row_names_the_knob_that_changes_it() {
        for s in Subject::ALL {
            let row = s.row();
            if row.tunable == Tunable::StoreFlagReadOnly {
                assert!(
                    row.note.contains("retentionPeriod"),
                    "{}: a read-only row must name the flag",
                    s.as_str()
                );
            }
        }
    }

    #[test]
    fn the_retention_flag_is_read_from_a_real_flags_body() {
        // Both bodies below were captured from the running test-server stack.
        assert_eq!(
            parse_retention_flag("-retentionPeriod=\"12\"\n").as_deref(),
            Some("12")
        );
        assert_eq!(
            parse_retention_flag(
                "-retentionPeriod=\"30d\"\n-storageDataPath=\"/victoria-logs-data\"\n"
            )
            .as_deref(),
            Some("30d")
        );
    }

    /// `/flags` lists only explicitly-set flags, so an absent line means the store is on its own
    /// default. That must read as "unknown", never as a number we made up.
    #[test]
    fn an_unset_flag_is_none_rather_than_a_guess() {
        assert_eq!(parse_retention_flag(""), None);
        assert_eq!(
            parse_retention_flag("-storageDataPath=\"/victoria-logs-data\"\n"),
            None
        );
        assert_eq!(parse_retention_flag("-retentionPeriod=\"\"\n"), None);
        // A flag whose name merely *contains* the needle is a different flag.
        assert_eq!(parse_retention_flag("-maxRetentionPeriod=\"9\"\n"), None);
    }

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
        ] {
            assert!(!bad.in_bounds(), "{bad:?} should be out of bounds");
        }
    }

    /// The field token is the name of a form control on the TypeScript side, so a duplicate would
    /// silently bind two rows to one input.
    #[test]
    fn every_token_set_is_unique() {
        let fields = [
            Field::AlertLinkedDays,
            Field::UnmatchedEventHours,
            Field::ReportRunDays,
            Field::FlowDays,
            Field::StoreOwned,
            Field::Unlimited,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for f in fields {
            assert!(seen.insert(f.as_str()), "duplicate field token");
        }
        let mut seen = std::collections::BTreeSet::new();
        for e in [
            Enforcement::PgPrune,
            Enforcement::StoreTtl,
            Enforcement::StoreFlag,
            Enforcement::Unlimited,
        ] {
            assert!(seen.insert(e.as_str()), "duplicate enforcement token");
        }
        let mut seen = std::collections::BTreeSet::new();
        for t in [
            Tunable::Settings,
            Tunable::StoreFlagReadOnly,
            Tunable::ByDecision,
        ] {
            assert!(seen.insert(t.as_str()), "duplicate tunable token");
        }
    }

    #[test]
    fn units_are_declared_for_every_editable_row() {
        for s in Subject::ALL {
            let row = s.row();
            if row.tunable == Tunable::Settings {
                assert!(!row.unit().is_empty(), "{} has no unit", s.as_str());
            }
        }
        assert_eq!(Subject::EventsUnmatched.row().unit(), "hours");
        assert_eq!(Subject::AlertHistory.row().unit(), "days");
    }
}
