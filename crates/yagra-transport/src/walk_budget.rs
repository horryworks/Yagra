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

/// How many per-round-trip timeouts one whole multi-column call may spend, when its caller did not
/// name a deadline of its own ([`WalkLimits::per_round_trip`]).
///
/// ⚠️ **This is not an operator's knob, and this doc used to say it was.** Core fills every SNMP
/// job's `timeout_ms` from one constant (`scheduler::SNMP_TIMEOUT_MS`, 2 s), and no profile or setting
/// overrides it, so "raise the check's timeout and the budget follows" was advice nobody could take.
/// The walk that needed a bigger budget — the interface table walk on a slow switch — now names its
/// own deadline from the poll interval ([`WalkLimits::until`], ADR-110 Increment 10). What still
/// comes here is the optical, adjacency and identity walks, whose budget is unchanged. A slow device
/// is otherwise answered in code, per caller — the identity probe's patch-table walk waits longer
/// for exactly this reason (ADR-138 Increment 4).
///
/// **Why eight.** The slowest *healthy* walk measured in this lab is 6.0 s (a 232-interface switch
/// over a LAN, recorded on the poller's `worker::table_plan::SINGLE_FLIGHT_FLOOR`). At the default
/// 2 s timeout this is a 16 s budget — 2.7× that worst case.
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
pub enum Truncation {
    /// The device failed [`MAX_CONSECUTIVE_COLUMN_FAILURES`] columns in a row.
    Silent,
    /// The whole call's deadline passed.
    Deadline,
}

impl Truncation {
    /// The `reason` label this appears under in [`note_truncation`]'s counter.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Deadline => "deadline",
        }
    }
}

/// One multi-column SNMP call's remaining patience.
///
/// Holds only the decision — no client, no columns, no I/O — so the stop conditions can be tested
/// without an agent. That matters more here than usual: most loops that consult it open a UDP
/// socket to port 161 and cannot be unit-tested, so this type carries most of the assertions the
/// workspace can make about the rule. The exception since ADR-110 Increment 9 is the v2c column
/// loop in `snmp.rs`, which runs against a scripted agent through its `BulkPager` seam.
pub(crate) struct WalkBudget {
    deadline: Instant,
    /// Whether the deadline also stops a column part-way — see [`Self::cuts_mid_column`].
    per_page: bool,
    consecutive_failures: usize,
    /// Columns recorded as [`ColumnOutcome::Answered`], for [`Self::every_column_answered`].
    answered: usize,
}

impl WalkBudget {
    /// A budget for a call whose per-round-trip timeout is `timeout`.
    pub(crate) fn new(timeout: Duration) -> Self {
        Self::with_remaining(timeout.saturating_mul(WALK_BUDGET_TIMEOUTS))
    }

    /// The budget a caller's [`WalkLimits`] describe (ADR-110 Increment 10).
    ///
    /// No deadline is exactly [`Self::new`]. A deadline replaces the multiple **and** is consulted
    /// before every page, not only before every column.
    pub(crate) fn within(limits: WalkLimits) -> Self {
        match limits.deadline {
            None => Self::new(limits.timeout),
            Some(deadline) => Self {
                deadline,
                per_page: true,
                consecutive_failures: 0,
                answered: 0,
            },
        }
    }

    /// A budget with an explicit amount of wall-clock left.
    ///
    /// The seam the tests build an already-spent budget through — `Duration::ZERO` expires it now.
    /// Subtracting from `Instant::now()` would be the other way to write that, and it is fallible
    /// on platforms whose monotonic clock starts at zero.
    pub(crate) fn with_remaining(remaining: Duration) -> Self {
        Self {
            deadline: Instant::now() + remaining,
            per_page: false,
            consecutive_failures: 0,
            answered: 0,
        }
    }

    /// Whether a column that has already been answered at least one page must stop before the next.
    ///
    /// 🚨 **Only when the caller named its deadline.** Before ADR-110 Increment 10 the deadline was
    /// consulted only at the top of a column, so a column that was started always finished — and a
    /// slow switch's walk overran its 16 s by whatever its last column cost, which is how some of
    /// GW01's polls reached their metric columns at all. Cutting at the page on that 16 s budget would
    /// have taken those polls away. The interface walk now sizes its deadline from the poll interval,
    /// and only a walk sized that way is cut here; every other walk keeps finishing what it started.
    pub(crate) fn cuts_mid_column(&self) -> bool {
        self.per_page && Instant::now() >= self.deadline
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
            ColumnOutcome::Answered => {
                self.consecutive_failures = 0;
                self.answered += 1;
            }
            ColumnOutcome::Failed => self.consecutive_failures += 1,
            ColumnOutcome::Skipped => {}
        }
    }

    /// Whether every one of the `asked` columns was recorded as answered — none failed, none was
    /// skipped, and the walk did not stop before reaching one (ADR-138 Increment 3).
    ///
    /// A different question from [`Self::spent`] and [`is_silence`], and it has to be: one column
    /// that times out between two that answer resets the silence count, so the walk returns `Ok`
    /// with the other columns' rows. A caller that pairs rows *across* columns — the Huawei patch
    /// table's version with its running state — would read that half-table as a device with no
    /// running patch. This is what lets it tell the two apart.
    pub(crate) fn every_column_answered(&self, asked: usize) -> bool {
        self.answered == asked
    }

    /// Whether the device answered **nothing** of what it was asked: at least one column was asked
    /// and not one was recorded as [`ColumnOutcome::Answered`] (ADR-138 Increment 5).
    ///
    /// A different question from [`Self::spent`]: [`Truncation::Silent`] needs two failures in a
    /// row, and the scalar GET most profiles issue asks for one OID (`sysUpTime.0`), so a silent
    /// device can never trip it there. This is what lets that GET tell "the agent answered, and
    /// implements none of these" — `Ok(vec![])`, a state the identity probe should still run in —
    /// from "nothing came back", which it must not spend a probe on. A [`ColumnOutcome::Skipped`]
    /// column neither accuses nor forgives, as in [`Self::record`]: a call whose every OID was
    /// malformed asked the device nothing, and this says `false`.
    pub(crate) fn heard_nothing(&self) -> bool {
        self.answered == 0 && self.consecutive_failures > 0
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

/// How many times one request may be sent again after it went unanswered (ADR-110 Increment 9).
///
/// **One.** A second silence on the same page is the device, not the packet.
pub(crate) const PAGE_RETRIES: u32 = 1;

/// How many re-sends one whole multi-column call may spend, across every page of every column.
///
/// Bounds the one case a retry makes more expensive: a device that answered the first columns and
/// then went quiet for good. Without a cap that device would pay two timeouts per page until the
/// deadline; with it, the extra cost is at most four timeouts, and the silence rule still ends the
/// walk two columns later.
pub(crate) const MAX_RETRIES_PER_WALK: u32 = 4;

/// Whether an unanswered request is worth asking again (ADR-110 Increment 9).
///
/// 🚨 **Why this exists.** A column used to be one `walk_bulk` call, and one GETBULK page that came
/// back later than the per-request timeout failed the **whole column** — discarding the rows it had
/// already paged. On a slow agent that drops the odd response (measured on the PoC: a manual walk
/// with no retries stopped at 20 of 229 `ifType` rows), two such columns in a row read as a silent
/// device, and a switch that was answering had its walk cut as `silent`.
///
/// 🚨 **Retrying is only allowed after the device has answered something in this walk, and that is
/// what keeps Increment 3's price for a silent device.** A device that never answers gets no retry
/// at all, so it still costs exactly two timeouts (one per column) before [`WalkBudget::spent`]
/// reports [`Truncation::Silent`]. Retrying the first page too would double that — and a mass
/// outage is when every device is in exactly that state.
pub(crate) struct RetryAllowance {
    spent: u32,
    heard: bool,
}

impl RetryAllowance {
    /// A fresh allowance for one multi-column call: nothing heard, nothing spent.
    pub(crate) fn new() -> Self {
        Self {
            spent: 0,
            heard: false,
        }
    }

    /// The device answered something in this walk — a page, a base GET, or an error PDU. An error
    /// PDU counts: bytes came back, so the agent is there (see `outcome_of`).
    pub(crate) fn heard(&mut self) {
        self.heard = true;
    }

    /// Whether the request that just ended with `outcome` should be sent again, given that it has
    /// already been re-sent `retries_on_this_request` times. Takes one from the allowance when it
    /// answers yes.
    ///
    /// Every condition is a reason to say no, and each is tested on its own:
    /// the outcome was an answer (never re-ask a device that replied), the device has not been
    /// heard from in this walk, this request has had its retry, the walk's allowance is spent, or
    /// the budget has run out.
    pub(crate) fn claim(
        &mut self,
        outcome: ColumnOutcome,
        retries_on_this_request: u32,
        budget: &WalkBudget,
    ) -> bool {
        let worth_it = outcome == ColumnOutcome::Failed
            && self.heard
            && retries_on_this_request < PAGE_RETRIES
            && self.spent < MAX_RETRIES_PER_WALK
            && budget.spent().is_none();
        if worth_it {
            self.spent += 1;
        }
        worth_it
    }
}

/// Record how a re-sent request ended: `recovered` when the retry was answered.
///
/// The counter is what says whether [`PAGE_RETRIES`] is doing anything on a real fleet — a
/// `recovered` count near zero means the retries only ever add time.
pub(crate) fn note_retry(recovered: bool) {
    metrics::counter!(
        "yagra_snmp_page_retries_total",
        "result" => if recovered { "recovered" } else { "failed" }
    )
    .increment(1);
}

/// Whether a finished multi-column call amounts to **"this device said nothing"**.
///
/// The judgement [`WalkBudget`] already made, handed to the caller instead of thrown away
/// (ADR-110 Increment 4). A caller that fires several walks at one device — `execute_mau` fires
/// three — otherwise pays the first walk's timeout again for each later one, because `Ok(vec![])`
/// cannot say whether the agent answered "I do not implement this" or did not answer at all.
///
/// **Both conditions carry weight:**
///
/// - **[`Truncation::Silent`], never [`Truncation::Deadline`].** A device that trips the deadline
///   is answering, slowly. Folding the two together would report a slow switch as an absent one —
///   and the two counters exist precisely because those want opposite responses.
/// - **No rows.** "Some columns answered and then it went quiet" is a partial read, not an absent
///   device, and what it did return is worth keeping.
///
/// Kept here for the reason [`WalkBudget`] gives about itself: the loops that consult it open a UDP
/// socket to port 161 and cannot be unit-tested, so every part of the rule a test can reach has to
/// live outside them.
pub(crate) fn is_silence(rows_collected: usize, stopped: Option<Truncation>) -> bool {
    rows_collected == 0 && matches!(stopped, Some(Truncation::Silent))
}

/// How long a multi-column table walk may run, as its caller states it (ADR-110 Increment 10).
///
/// Two shapes, and the difference is more than the number:
///
/// | | budget | checked |
/// |---|---|---|
/// | [`Self::per_round_trip`] | [`WALK_BUDGET_TIMEOUTS`] × `timeout`, from when the walk starts | before each column |
/// | [`Self::until`] | the caller's instant | before each column **and each page** |
///
/// The first is what every walk had before Increment 10, and what the optical, adjacency, identity
/// and discovery walks keep. The second is for a caller that knows how long it can afford — the
/// interface table walk, which sizes one budget for its whole job from the poll interval and hands
/// each of its two walks a share of it. Why the page check comes only with it is on
/// [`WalkBudget::cuts_mid_column`].
///
/// ⚠️ **A page already sent is not recalled.** The deadline stops the *next* request, so a walk can
/// overrun it by one round trip (and its one retry, which is only granted while the deadline has
/// not passed). Whoever waits behind a walk has to allow for that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkLimits {
    /// How long one request waits for its answer.
    pub timeout: Duration,
    /// When the whole walk must stop. `None` is [`Self::per_round_trip`].
    pub deadline: Option<Instant>,
}

impl WalkLimits {
    /// Increment 3's budget: [`WALK_BUDGET_TIMEOUTS`] round trips, consulted between columns.
    #[must_use]
    pub fn per_round_trip(timeout: Duration) -> Self {
        Self {
            timeout,
            deadline: None,
        }
    }

    /// Stop at `deadline`, including part-way down a column.
    #[must_use]
    pub fn until(timeout: Duration, deadline: Instant) -> Self {
        Self {
            timeout,
            deadline: Some(deadline),
        }
    }
}

/// How one column of a table walk ended (ADR-110 Increment 10).
///
/// The rows alone cannot say this, for the reason [`crate::InstanceWalk`] gives: fewer rows is what
/// both a device that does not implement a column and a walk that never reached it look like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnEnd {
    /// The agent walked it to its end — **including by answering that it does not implement it**,
    /// or with an error PDU. Either is an answer (see this module's doc).
    Answered,
    /// The deadline fell while this column was being paged. The rows before the cut are in the walk;
    /// the column continues after the instance `resume_after` (the sub-identifiers past its base —
    /// empty when not one row had arrived yet).
    Partial { resume_after: Vec<u32> },
    /// The agent stopped answering part-way, and its retry went unanswered too. Rows it gave before
    /// that are kept.
    Failed,
    /// Nothing was asked: the column OID is malformed.
    Skipped,
    /// The walk stopped before reaching this column.
    NotAsked,
}

/// One column of a table walk and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnReport {
    /// The column base OID as the caller spelled it.
    pub column: String,
    pub end: ColumnEnd,
}

/// What a numeric or string table walk collected, column by column (ADR-110 Increment 10).
#[derive(Debug, Clone, PartialEq)]
pub struct TableWalk<R> {
    pub rows: Vec<R>,
    /// One entry per column asked for, in the order asked.
    pub columns: Vec<ColumnReport>,
    /// Why the walk stopped short, if it did — what `Transport::snmp_walk` returned as its second
    /// half before this type existed.
    pub stopped: Option<Truncation>,
}

/// How one column's conversation ended, as the walker loops need it: the verdict about the device
/// that [`WalkBudget::record`] folds in, or a cut at the deadline part-way down the column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ColumnStop {
    Ended(ColumnOutcome),
    /// Cut by [`WalkBudget::cuts_mid_column`] after the instance this carries.
    Cut(Vec<u32>),
}

impl ColumnStop {
    /// What this says about the device. A cut column had answered every page it was sent, so it is
    /// an answer — it must not count toward [`MAX_CONSECUTIVE_COLUMN_FAILURES`].
    pub(crate) fn outcome(&self) -> ColumnOutcome {
        match self {
            Self::Ended(outcome) => *outcome,
            Self::Cut(_) => ColumnOutcome::Answered,
        }
    }

    /// The public report for this column.
    pub(crate) fn end(self) -> ColumnEnd {
        match self {
            Self::Ended(ColumnOutcome::Answered) => ColumnEnd::Answered,
            Self::Ended(ColumnOutcome::Failed) => ColumnEnd::Failed,
            Self::Ended(ColumnOutcome::Skipped) => ColumnEnd::Skipped,
            Self::Cut(resume_after) => ColumnEnd::Partial { resume_after },
        }
    }
}

/// Why a table walk that ran out of columns stopped short, if it did.
///
/// The column loops only consult the budget at the top of a column, so a walk whose last columns
/// failed ends by running out of columns rather than by tripping, and has to be asked once more.
/// **Two answers, and the second is narrower than [`WalkBudget::spent`] on purpose:** silence is the
/// device's run of failures, but a deadline counts only when a column was actually cut. A last
/// column that finished just after the deadline has asked for everything — reading the clock
/// instead reported that walk as truncated, and `snmp_walk_complete` said `0` for a whole table.
pub(crate) fn trailing_stop(budget: &WalkBudget, columns: &[ColumnReport]) -> Option<Truncation> {
    if budget.consecutive_failures >= MAX_CONSECUTIVE_COLUMN_FAILURES {
        return Some(Truncation::Silent);
    }
    columns
        .iter()
        .any(|c| matches!(c.end, ColumnEnd::Partial { .. }))
        .then_some(Truncation::Deadline)
}

/// The reports for the columns a table walk stopped before reaching.
pub(crate) fn not_asked(rest: &[String]) -> impl Iterator<Item = ColumnReport> + '_ {
    rest.iter().map(|column| ColumnReport {
        column: column.clone(),
        end: ColumnEnd::NotAsked,
    })
}

/// Why a finished table walk stopped short: the reason its column loop broke on, or else
/// [`trailing_stop`]. A last column cut part-way is noted here, because no loop break reported it —
/// nothing was left unasked (`skipped = 0`), but that column was not read out.
pub(crate) fn conclude(
    broke_on: Option<Truncation>,
    budget: &WalkBudget,
    columns: &[ColumnReport],
    target: IpAddr,
) -> Option<Truncation> {
    broke_on.or_else(|| {
        let trailing = trailing_stop(budget, columns);
        if trailing == Some(Truncation::Deadline) {
            note_truncation(Truncation::Deadline, target, 0);
        }
        trailing
    })
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
                // Either constructor: `within` is how a table walk takes the caller's deadline
                // (ADR-110 Increment 10), and it falls back to `new` when there is none.
                assert!(
                    body.contains("WalkBudget::new(") || body.contains("WalkBudget::within("),
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

    /// **Every scalar GET reports a device that answered nothing** (ADR-138 Increment 5) — and,
    /// as above, reading the source is all there is: both GET loops call the real client.
    ///
    /// The identity probe rides the scalar GET and runs whenever the agent answered, so a GET that
    /// folded "nothing came back" into `Ok(vec![])` again would send that probe at every silent
    /// device — the cost [`MAX_CONSECUTIVE_COLUMN_FAILURES`] exists to avoid. The shape matched is
    /// the one both loops share: a list of OIDs in, a list of samples out; the walks return a
    /// `TableWalk` and the v3 string GET a different sample type, so neither is examined here.
    #[test]
    fn every_scalar_get_reports_a_device_that_answered_nothing() {
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
                if !body.contains("oids: &[String]") || !body.contains("Result<Vec<SnmpSample>") {
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
                    body.contains("heard_nothing()") && body.contains("TransportError::Silent("),
                    "yagra-transport/src/{name}: `{signature}` answers `Ok(vec![])` for a device \
                     that answered nothing, and the identity probe would then be sent to it \
                     (ADR-138 Increment 5)"
                );
            }
        }
        assert_eq!(
            checked, 2,
            "the two scalar GETs, v2c and v3, are what this crate has; {checked} were examined"
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
        assert!(
            !budget.heard_nothing(),
            "nothing asked is not nothing answered"
        );
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
        // …but for a call that asked one thing — the scalar GET of most profiles — one failure is
        // already everything unanswered (ADR-138 Increment 5).
        assert!(budget.heard_nothing());
        budget.record(ColumnOutcome::Failed);
        assert_eq!(budget.spent(), Some(Truncation::Silent));
        assert!(budget.heard_nothing());
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
        assert!(
            !budget.heard_nothing(),
            "one answer is an agent that is there"
        );
        budget.record(ColumnOutcome::Failed);
        assert_eq!(
            budget.spent(),
            None,
            "two failures with a success between them are two columns, not a silent device"
        );
        budget.record(ColumnOutcome::Failed);
        assert_eq!(budget.spent(), Some(Truncation::Silent), "…now they are");
        assert!(
            !budget.heard_nothing(),
            "silence by the run is a different question: this device did answer once"
        );
    }

    /// A column nothing was asked of says nothing about the device, in either direction.
    #[test]
    fn a_skipped_column_neither_accuses_the_device_nor_forgives_it() {
        let mut budget = WalkBudget::new(Duration::from_secs(2));
        budget.record(ColumnOutcome::Skipped);
        assert!(
            !budget.heard_nothing(),
            "a malformed OID asked the device nothing"
        );
        budget.record(ColumnOutcome::Failed);
        budget.record(ColumnOutcome::Skipped);
        assert_eq!(
            budget.spent(),
            None,
            "a malformed OID is not a second failure"
        );
        assert!(budget.heard_nothing(), "…and it is not an answer either");
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

    /// **The accepting side first**: a walk whose every column answered says so. A rule answering
    /// `false` unconditionally would pass every test below it — and would leave every Huawei device
    /// with no OS version at all, which is a quieter failure than the one it was written to fix.
    #[test]
    fn a_walk_whose_every_column_answered_says_so() {
        let mut budget = WalkBudget::new(Duration::from_secs(2));
        budget.record(ColumnOutcome::Answered);
        budget.record(ColumnOutcome::Answered);
        assert!(budget.every_column_answered(2));
        assert!(
            WalkBudget::new(Duration::from_secs(2)).every_column_answered(0),
            "asking for nothing leaves nothing unanswered"
        );
    }

    /// The case ADR-138 Increment 3 exists for: a column times out between two that answer. The
    /// silence count resets — the walk goes on and returns `Ok` — but the walk is not whole.
    #[test]
    fn a_failed_column_between_answers_leaves_the_walk_incomplete() {
        let mut budget = WalkBudget::new(Duration::from_secs(2));
        budget.record(ColumnOutcome::Answered);
        budget.record(ColumnOutcome::Failed);
        budget.record(ColumnOutcome::Answered);
        assert_eq!(budget.spent(), None, "the walk itself carries on");
        assert!(!budget.every_column_answered(3));
    }

    /// A column never reached — the deadline, silence or the row cap stopped the loop first — and a
    /// column skipped for a malformed OID are both columns nobody heard from.
    #[test]
    fn a_column_never_reached_or_skipped_is_not_answered() {
        let mut stopped_early = WalkBudget::new(Duration::from_secs(2));
        stopped_early.record(ColumnOutcome::Answered);
        assert!(!stopped_early.every_column_answered(2));

        let mut skipped = WalkBudget::new(Duration::from_secs(2));
        skipped.record(ColumnOutcome::Answered);
        skipped.record(ColumnOutcome::Skipped);
        assert!(!skipped.every_column_answered(2));
    }

    /// The two reasons are distinct labels, because the counter is read to tell them apart.
    #[test]
    fn the_two_truncation_reasons_are_labelled_apart() {
        assert_ne!(Truncation::Silent.reason(), Truncation::Deadline.reason());
    }

    /// **The accepting side of [`is_silence`], and it comes first on purpose.**
    ///
    /// Every other assertion about this rule is of the form "that is silence". A predicate
    /// answering `true` unconditionally satisfies all of them — and would turn every adjacency,
    /// MAU and ENTITY walk on every healthy device into an error, silently emptying the network
    /// map while this module stayed green
    /// (`rejection-only-tests-pass-when-everything-rejects`).
    #[test]
    fn a_walk_that_collected_rows_is_never_silence() {
        assert!(!is_silence(1, Some(Truncation::Silent)));
        assert!(!is_silence(4096, Some(Truncation::Silent)));
        assert!(
            !is_silence(0, None),
            "a walk that simply ran out of columns is not a statement about the device"
        );
        assert!(!is_silence(7, None));
    }

    /// Only a silent truncation with nothing collected is the device.
    ///
    /// The `Deadline` half is the one that would be easy to get wrong: a slow device answers, and
    /// calling it absent would make `WALK_BUDGET_TIMEOUTS` being too small look like a fleet of
    /// unreachable switches.
    #[test]
    fn only_a_silent_truncation_with_no_rows_is_silence() {
        assert!(is_silence(0, Some(Truncation::Silent)));
        assert!(!is_silence(0, Some(Truncation::Deadline)));
    }

    // ── RetryAllowance (ADR-110 Increment 9) ──────────────────────────────────

    /// **The accepting side, first**: a device that has answered gets its unanswered page asked
    /// once more — and only once. A `claim` that answered `false` unconditionally would pass every
    /// test after this one.
    #[test]
    fn a_timeout_after_the_device_answered_is_retried_once() {
        let budget = WalkBudget::new(Duration::from_secs(2));
        let mut retries = RetryAllowance::new();
        retries.heard();
        assert!(retries.claim(ColumnOutcome::Failed, 0, &budget));
        assert!(
            !retries.claim(ColumnOutcome::Failed, PAGE_RETRIES, &budget),
            "a page that stayed silent through its retry is the device, not the packet"
        );
    }

    /// 🚨 The rule that keeps Increment 3's price for a silent device: nothing heard, no retry.
    #[test]
    fn a_timeout_before_the_device_ever_answered_is_not_retried() {
        let budget = WalkBudget::new(Duration::from_secs(2));
        let mut retries = RetryAllowance::new();
        assert!(!retries.claim(ColumnOutcome::Failed, 0, &budget));
    }

    /// An answer is never re-asked — including a column skipped for a malformed OID, which sent
    /// nothing to retry.
    #[test]
    fn an_answer_or_a_skip_is_never_retried() {
        let budget = WalkBudget::new(Duration::from_secs(2));
        let mut retries = RetryAllowance::new();
        retries.heard();
        assert!(!retries.claim(ColumnOutcome::Answered, 0, &budget));
        assert!(!retries.claim(ColumnOutcome::Skipped, 0, &budget));
    }

    /// The allowance is per walk and runs out: a device that answered and then went quiet for good
    /// pays at most [`MAX_RETRIES_PER_WALK`] extra timeouts.
    #[test]
    fn the_walk_retry_allowance_is_bounded() {
        let budget = WalkBudget::new(Duration::from_secs(2));
        let mut retries = RetryAllowance::new();
        retries.heard();
        for page in 0..MAX_RETRIES_PER_WALK {
            assert!(
                retries.claim(ColumnOutcome::Failed, 0, &budget),
                "page {page} is within the allowance"
            );
        }
        assert!(!retries.claim(ColumnOutcome::Failed, 0, &budget));
    }

    /// A retry never outlives the budget: once the deadline has passed, nothing is sent again.
    #[test]
    fn no_retry_past_the_deadline() {
        let budget = WalkBudget::with_remaining(Duration::ZERO);
        let mut retries = RetryAllowance::new();
        retries.heard();
        assert!(!retries.claim(ColumnOutcome::Failed, 0, &budget));
    }

    // ── WalkLimits (ADR-110 Increment 10) ─────────────────────────────────────

    /// **The accepting side, first**: a deadline the caller named and that has not passed lets the
    /// walk start and lets a column go on to its next page. A budget that stopped everything would
    /// pass every test after this one.
    #[test]
    fn a_named_deadline_that_has_not_passed_lets_the_walk_page_on() {
        let later = Instant::now() + Duration::from_secs(30);
        let budget = WalkBudget::within(WalkLimits::until(Duration::from_secs(2), later));
        assert_eq!(budget.spent(), None);
        assert!(!budget.cuts_mid_column());
        assert!(
            budget.remaining() > Duration::from_secs(20),
            "the caller's deadline replaces the 8 × timeout multiple, got {:?}",
            budget.remaining()
        );
    }

    /// A deadline the caller named cuts a column part-way once it has passed.
    #[test]
    fn a_named_deadline_that_has_passed_cuts_the_column_part_way() {
        let budget = WalkBudget::within(WalkLimits::until(Duration::from_secs(2), Instant::now()));
        assert_eq!(budget.spent(), Some(Truncation::Deadline));
        assert!(budget.cuts_mid_column());
    }

    /// 🚨 **Without a named deadline, a column that was started always finishes** — even once the
    /// budget has run out. This is what keeps the optical, adjacency and identity walks exactly as
    /// they were: cutting at the page on their 16 s budget would take away the polls that used to
    /// finish by overrunning it (see [`WalkBudget::cuts_mid_column`]).
    #[test]
    fn without_a_named_deadline_a_started_column_is_never_cut() {
        let expired = WalkBudget::with_remaining(Duration::ZERO);
        assert_eq!(expired.spent(), Some(Truncation::Deadline));
        assert!(!expired.cuts_mid_column());

        let legacy = WalkBudget::within(WalkLimits::per_round_trip(Duration::from_secs(2)));
        let remaining = legacy.remaining();
        assert!(
            remaining > Duration::from_millis(15_900) && remaining <= Duration::from_secs(16),
            "no deadline is Increment 3's budget, got {remaining:?}"
        );
    }

    /// A cut column had its every page answered, so it is an answer: two cuts in a row are not a
    /// silent device.
    #[test]
    fn a_cut_column_is_an_answer_not_a_failure() {
        let cut = ColumnStop::Cut(vec![7]);
        assert_eq!(cut.outcome(), ColumnOutcome::Answered);
        assert_eq!(
            cut.end(),
            ColumnEnd::Partial {
                resume_after: vec![7]
            }
        );
        let mut budget = WalkBudget::new(Duration::from_secs(2));
        budget.record(ColumnStop::Cut(vec![1]).outcome());
        budget.record(ColumnStop::Cut(vec![2]).outcome());
        assert_eq!(budget.spent(), None);
    }

    fn report(end: ColumnEnd) -> ColumnReport {
        ColumnReport {
            column: "1.3.6.1.2.1.2.2.1.8".to_owned(),
            end,
        }
    }

    /// **The accepting side of [`trailing_stop`]**: a walk that asked for every column is not
    /// truncated — even when its budget ran out while the last one was being read. Reading the clock
    /// here used to report exactly that walk as cut.
    #[test]
    fn a_walk_that_asked_every_column_is_not_truncated_even_past_its_deadline() {
        let expired = WalkBudget::with_remaining(Duration::ZERO);
        let columns = [report(ColumnEnd::Answered), report(ColumnEnd::Failed)];
        assert_eq!(trailing_stop(&expired, &columns), None);
    }

    /// A column cut part-way is a deadline, and a run of failures is silence, which is named first.
    #[test]
    fn a_trailing_cut_is_a_deadline_and_a_trailing_run_of_failures_is_silence() {
        let budget = WalkBudget::new(Duration::from_secs(2));
        let cut = [
            report(ColumnEnd::Answered),
            report(ColumnEnd::Partial {
                resume_after: vec![3],
            }),
        ];
        assert_eq!(trailing_stop(&budget, &cut), Some(Truncation::Deadline));

        let mut quiet = WalkBudget::new(Duration::from_secs(2));
        quiet.record(ColumnOutcome::Failed);
        quiet.record(ColumnOutcome::Failed);
        assert_eq!(trailing_stop(&quiet, &cut), Some(Truncation::Silent));
    }
}
