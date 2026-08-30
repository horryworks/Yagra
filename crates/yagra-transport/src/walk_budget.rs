// SPDX-License-Identifier: AGPL-3.0-only
//! What one multi-column SNMP call is allowed to spend (ADR-110 Increment 3).
//!
//! Every SNMP walker here is a loop over columns, and each column is its own conversation with its
//! own per-round-trip timeout. Nothing bounded the loop, so **a device that answers nothing paid
//! the timeout once per column**: eighteen columns at two seconds each is thirty-six seconds of a
//! poller's concurrency permit spent on one fact — that the device is not there. Measured at
//! **51,299 ms** on a poller that had been detached from the network its agents were on.
//!
//! That is a capacity defect, not a latency one. ADR-109 established the poller's ceiling as
//! `permits / probe time`, so against a healthy table walk of 513 ms one silent device costs the
//! permit it holds a hundred polls. 🚨 **And it degrades hardest exactly when it matters** — in a
//! mass outage many devices go silent at once, so the poller's throughput collapses in the middle
//! of the incident it exists to report.
//!
//! ## Two ways to stop, answering two different failures
//!
//! | | answers |
//! |---|---|
//! | [`MAX_CONSECUTIVE_COLUMN_FAILURES`] | **a silent device** — the measured case |
//! | [`WALK_BUDGET_TIMEOUTS`] | **a device that answers, slowly, forever** — previously unbounded |
//!
//! The first is what makes the common failure cheap; the second is the guarantee. They are kept
//! apart, and counted apart, because a fleet where the *deadline* fires is a fleet whose multiplier
//! is wrong, while a fleet where *silence* fires is a fleet with unreachable devices. One counter
//! could not tell those two operators apart.
//!
//! ## 🚨 Why counting consecutive failures is safe
//!
//! It rests on a property of the client, and the whole design is wrong without it: **a column the
//! device does not implement is not a failure.** `csnmp`'s `walk_bulk` walks out of the subtree,
//! finds nothing, issues one `GET` on the column base, and **swallows `noSuchObject` /
//! `noSuchInstance`, returning `Ok(empty)`**. The v3 walker reaches the same place through
//! `EndOfMibView` / `NoSuchObject` varbind values, which end that column normally.
//!
//! Measured confirmation: `.210`'s healthy table walk asks for twenty columns — five of them vendor
//! columns most of its devices do not have — and takes **513 ms**. If an unimplemented column
//! errored, that walk would take forty seconds.
//!
//! So two consecutive `Failed` columns with no success between them is a statement about the
//! **device**, not about the columns. One is not: a single column can error for its own reasons, and
//! stopping a whole walk on it would be a new way to lose data.

use std::net::IpAddr;
use std::time::{Duration, Instant};

/// How many columns in a row may fail before the walk stops asking this device anything.
///
/// **Two, not one.** One column can fail for its own reason — an agent that answers an error PDU
/// for a subtree rather than an empty page — and ending the whole walk there would trade a bounded
/// cost for a silent loss of every column after it. Two in a row, with no success between, is the
/// device.
///
/// ⚠️ Read this with the module doc's safety argument: it counts columns the device **failed to
/// answer**, never columns it does not implement.
pub(crate) const MAX_CONSECUTIVE_COLUMN_FAILURES: usize = 2;

/// How many per-round-trip timeouts one whole multi-column call may spend.
///
/// **Deliberately not a new setting.** The caller already says how patient it is, once per check, as
/// `timeout_ms`; the whole call's patience is a fixed multiple of that. An operator with a device
/// slow enough to be truncated therefore already has the lever — raising that check's `timeout_ms`
/// raises this budget with it — and nobody has to learn a second knob whose right value depends on
/// the first.
///
/// **Why eight.** The slowest *healthy* walk measured in this lab is 6.0 s (a 232-interface switch
/// over a LAN, recorded on `worker::stream`'s `MAX_SINGLE_FLIGHT_WAIT`). At the default 2 s timeout
/// this is a 16 s budget — 2.7× that worst case.
///
/// ⚠️ **That headroom is an argument, not a measurement of the fleet.** A 200-port device across a
/// 100 ms WAN plausibly needs longer and would be truncated. [`Truncation::Deadline`] exists to say
/// so out loud: if a healthy deployment ever reports it, this number is wrong, not the device.
pub(crate) const WALK_BUDGET_TIMEOUTS: u32 = 8;

/// What one column's conversation did, from the budget's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColumnOutcome {
    /// The agent answered. **Including "I do not implement this column"** — that is an answer, and
    /// the module doc explains why the whole design depends on it being counted as one.
    Answered,
    /// The request errored or timed out: the agent said nothing at all.
    Failed,
    /// Nothing was asked — a malformed column OID. Not evidence about the device either way, so it
    /// neither accuses it nor forgives it.
    Skipped,
}

/// Why a walk stopped before it ran out of columns.
///
/// Two variants rather than one boolean because they want opposite responses: [`Self::Silent`] is
/// the mechanism working (an unreachable device, established cheaply), while [`Self::Deadline`] on
/// a healthy fleet means [`WALK_BUDGET_TIMEOUTS`] is too small.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Truncation {
    /// The device failed [`MAX_CONSECUTIVE_COLUMN_FAILURES`] columns in a row.
    Silent,
    /// The whole call's deadline passed.
    Deadline,
}

impl Truncation {
    /// The `reason` label this appears under in [`note_truncation`]'s counter.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Deadline => "deadline",
        }
    }
}

/// One multi-column SNMP call's remaining patience.
///
/// Holds only the decision — no client, no columns, no I/O — so the stop conditions can be tested
/// without an agent. That matters more here than usual: the loops that consult it cannot be
/// unit-tested at all (they need a real device on a fixed UDP port), so this type carries every
/// assertion the workspace is able to make about the rule.
pub(crate) struct WalkBudget {
    deadline: Instant,
    consecutive_failures: usize,
}

impl WalkBudget {
    /// A budget for a call whose per-round-trip timeout is `timeout`.
    pub(crate) fn new(timeout: Duration) -> Self {
        Self::with_remaining(timeout.saturating_mul(WALK_BUDGET_TIMEOUTS))
    }

    /// A budget with an explicit amount of wall-clock left.
    ///
    /// The seam the tests build an already-spent budget through — `Duration::ZERO` expires it now.
    /// Subtracting from `Instant::now()` would be the other way to write that, and it is fallible
    /// on platforms whose monotonic clock starts at zero.
    fn with_remaining(remaining: Duration) -> Self {
        Self {
            deadline: Instant::now() + remaining,
            consecutive_failures: 0,
        }
    }

    /// Why this walk must stop, or `None` to attempt another column.
    ///
    /// Silence is reported ahead of the deadline: when both hold, the device being unreachable is
    /// the more useful of the two things to have said.
    pub(crate) fn spent(&self) -> Option<Truncation> {
        if self.consecutive_failures >= MAX_CONSECUTIVE_COLUMN_FAILURES {
            return Some(Truncation::Silent);
        }
        (Instant::now() >= self.deadline).then_some(Truncation::Deadline)
    }

    /// Fold one column's result in.
    ///
    /// 🚨 **[`ColumnOutcome::Answered`] resets the run to zero**, and that is the half a
    /// rejection-only test cannot see: without it the rule becomes "any two failures anywhere in the
    /// walk", which cuts a healthy twenty-column device that has two columns its agent errors on —
    /// and cuts it silently, because the poll still succeeds with fewer samples.
    pub(crate) fn record(&mut self, outcome: ColumnOutcome) {
        match outcome {
            ColumnOutcome::Answered => self.consecutive_failures = 0,
            ColumnOutcome::Failed => self.consecutive_failures += 1,
            ColumnOutcome::Skipped => {}
        }
    }

    /// Wall-clock left before the deadline, saturating at zero.
    ///
    /// Test-only: the walkers ask [`Self::spent`], never how much is left. It exists so
    /// `the_budget_is_a_multiple_of_the_callers_timeout` can see the one knob this design has.
    #[cfg(test)]
    pub(crate) fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

/// Record one truncated call: a counter for the fleet, a warning for the operator.
///
/// 🚨 **The counter is what makes [`WALK_BUDGET_TIMEOUTS`] falsifiable.** Truncation is otherwise
/// invisible from outside — the poll still returns, with fewer samples — so without this the
/// multiplier would be a number nobody could ever check. `reason="deadline"` on a healthy fleet is
/// the signal that it is wrong.
///
/// `skipped` is how many columns were never attempted, so the warning distinguishes "gave up on the
/// last one" from "gave up on sixteen".
pub(crate) fn note_truncation(reason: Truncation, target: IpAddr, skipped: usize) {
    metrics::counter!("yagra_snmp_walk_truncated_total", "reason" => reason.reason()).increment(1);
    tracing::warn!(
        %target,
        reason = reason.reason(),
        skipped,
        "snmp walk truncated: the remaining columns were not attempted"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Every multi-column SNMP call takes a budget** — and this check is all there is.
    ///
    /// 🚨 The nine loops that consult [`WalkBudget`] cannot be unit-tested. Each one opens a UDP
    /// socket to port 161 of an address the caller names, so exercising one needs a real agent on a
    /// privileged port; the tests above cover the *decision*, and nothing covers the *wiring*.
    /// Reading the source is the only technique left, which is why this crate grew a
    /// `module_source` for it.
    ///
    /// 🚨 **The floor is the load-bearing half.** The healthy answer here is "found nothing wrong",
    /// which is indistinguishable from "matched nothing" — a renamed parameter or a signature
    /// change would leave this passing over zero functions. Nine is what the crate has: four v2c,
    /// five v3.
    #[test]
    fn every_multi_column_call_takes_a_budget() {
        use crate::module_source::{files_no_comments, roots};

        let mut files = files_no_comments(&roots("src", "snmp"));
        files.extend(files_no_comments(&roots("src", "snmp_v3")));

        let mut checked = 0usize;
        for (name, code) in &files {
            for (at, _) in code.match_indices("pub async fn ") {
                let body = &code[at..];
                let end = body
                    .find("\n}")
                    .expect("a top-level fn closes with a brace at column zero");
                let body = &body[..end];
                // The multi-column calls are exactly the ones handed a *list* of OIDs. Matching on
                // the parameter rather than on a list of names means a tenth one is caught by
                // having the shape, not by someone remembering to add it here.
                if !body.contains("oids: &[String]") {
                    continue;
                }
                let signature = body
                    .lines()
                    .next()
                    .unwrap_or(body)
                    .trim_end_matches('(')
                    .to_owned();
                checked += 1;
                assert!(
                    body.contains("WalkBudget::new("),
                    "yagra-transport/src/{name}: `{signature}` walks a list of columns without a \
                     budget. A device that answers nothing then costs one timeout per column — the \
                     defect ADR-110 Increment 3 exists to close, measured at 51,299 ms"
                );
            }
        }
        assert!(
            checked >= 9,
            "only {checked} multi-column calls were examined across the two SNMP files; the \
             assertion above ran over almost nothing. Four v2c and five v3 is what this crate has"
        );
    }

    /// **The accepting side, and it comes first on purpose.**
    ///
    /// Every other assertion here is of the form "the walk stops". A budget that stopped
    /// unconditionally would satisfy all of them, and the walkers would then collect nothing from
    /// any device while the suite stayed green
    /// (`rejection-only-tests-pass-when-everything-rejects`).
    #[test]
    fn a_fresh_budget_permits_the_first_column() {
        let budget = WalkBudget::new(Duration::from_secs(2));
        assert_eq!(budget.spent(), None);
        assert!(budget.remaining() > Duration::from_secs(1));
    }

    /// The measured failure: a device that answers nothing costs two columns, not eighteen.
    #[test]
    fn two_consecutive_failures_stop_the_walk() {
        let mut budget = WalkBudget::new(Duration::from_secs(2));
        budget.record(ColumnOutcome::Failed);
        assert_eq!(
            budget.spent(),
            None,
            "one failure is a column, not a device"
        );
        budget.record(ColumnOutcome::Failed);
        assert_eq!(budget.spent(), Some(Truncation::Silent));
    }

    /// 🚨 A success between two failures resets the run.
    ///
    /// Without this the rule would be "any two failures in the whole walk", which cuts a healthy
    /// twenty-column device whose agent errors on two of them — and every other test in this module
    /// passes against that wrong rule.
    #[test]
    fn a_success_between_two_failures_resets_the_count() {
        let mut budget = WalkBudget::new(Duration::from_secs(2));
        budget.record(ColumnOutcome::Failed);
        budget.record(ColumnOutcome::Answered);
        budget.record(ColumnOutcome::Failed);
        assert_eq!(
            budget.spent(),
            None,
            "two failures with a success between them are two columns, not a silent device"
        );
        budget.record(ColumnOutcome::Failed);
        assert_eq!(budget.spent(), Some(Truncation::Silent), "…now they are");
    }

    /// A column nothing was asked of says nothing about the device, in either direction.
    #[test]
    fn a_skipped_column_neither_accuses_the_device_nor_forgives_it() {
        let mut budget = WalkBudget::new(Duration::from_secs(2));
        budget.record(ColumnOutcome::Failed);
        budget.record(ColumnOutcome::Skipped);
        assert_eq!(
            budget.spent(),
            None,
            "a malformed OID is not a second failure"
        );
        budget.record(ColumnOutcome::Failed);
        assert_eq!(
            budget.spent(),
            Some(Truncation::Silent),
            "…and it did not clear the first one either"
        );
    }

    /// The outer bound, for the device that answers every column slowly enough to never fail.
    #[test]
    fn an_expired_deadline_stops_the_walk() {
        let budget = WalkBudget::with_remaining(Duration::ZERO);
        assert_eq!(budget.spent(), Some(Truncation::Deadline));
        assert_eq!(budget.remaining(), Duration::ZERO);
    }

    /// Silence is named ahead of the deadline when both hold.
    ///
    /// Not cosmetic: the two reasons want opposite responses from whoever reads the counter, so a
    /// silent device reported as a deadline breach would read as "the multiplier is too small".
    #[test]
    fn silence_is_named_ahead_of_the_deadline() {
        let mut budget = WalkBudget::with_remaining(Duration::ZERO);
        budget.record(ColumnOutcome::Failed);
        budget.record(ColumnOutcome::Failed);
        assert_eq!(budget.spent(), Some(Truncation::Silent));
    }

    /// The budget is the caller's timeout times the constant — which is the whole escape hatch.
    ///
    /// A deployment whose devices are slower than this lab's has no new setting to find: it raises
    /// that check's `timeout_ms` and the budget follows. If this stopped being a multiple, that
    /// advice would silently become wrong.
    #[test]
    fn the_budget_is_a_multiple_of_the_callers_timeout() {
        let default = WalkBudget::new(Duration::from_secs(2)).remaining();
        let patient = WalkBudget::new(Duration::from_secs(5)).remaining();
        // A range rather than an equality: `remaining()` reads the clock, so a few microseconds
        // have already gone by the time it is called.
        assert!(
            default > Duration::from_millis(15_900) && default <= Duration::from_secs(16),
            "2s × {WALK_BUDGET_TIMEOUTS} should be the budget, got {default:?}"
        );
        assert!(
            patient > Duration::from_millis(39_900) && patient <= Duration::from_secs(40),
            "raising a check's timeout must raise its budget — that is the only knob, got \
             {patient:?}"
        );
    }

    /// The two reasons are distinct labels, because the counter is read to tell them apart.
    #[test]
    fn the_two_truncation_reasons_are_labelled_apart() {
        assert_ne!(Truncation::Silent.reason(), Truncation::Deadline.reason());
    }
}
