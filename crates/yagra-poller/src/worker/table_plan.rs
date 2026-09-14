// SPDX-License-Identifier: AGPL-3.0-only
//! How long one interface table job may run, and how long a job waits behind another on the same
//! device — both decided from the poll interval the job already carries (ADR-110 Increment 10).
//!
//! Pure: an interval in, durations out. No clock is read here except where a caller hands one in,
//! so the arithmetic is tested without a device or a timer.
//!
//! 🚨 **Why the interval and not a setting.** Before this, a table walk's budget was the per-request
//! timeout × 8 — 16 s at the 2 s every job carries — whatever the interval, and each of the job's two
//! walks had one of its own. A slow switch polled every minute (measured on the PoC: GW01, 75 rows)
//! lost six to ten columns on half its polls while the minute had room to spare, and nothing an
//! operator could reach changed that. The interval is what says how long a poll can afford, and it is
//! already on every job (`PollJob::interval_secs`), so the **bus does not change** and a core of
//! either version works with this poller.

use std::time::{Duration, Instant};

/// The smallest budget a table job gets: what one walk had before Increment 10 (2 s × 8), and what
/// an interval of 32 s or less — or an operator's "poll now", which carries interval 0 — still gets.
pub(super) const TABLE_BUDGET_FLOOR: Duration = Duration::from_secs(16);

/// The largest budget a table job gets, reached at a 300 s interval.
///
/// A cap because a walk this long is holding a concurrency permit the whole time. At 150 s a device
/// polled every five minutes can be walked for half of that — enough for everything except a device
/// broken the way TDC1004MD01 is (Increment 6 measured it at 26 rows/s against 809–2,209 for its
/// siblings), which Increment 12 answers by continuing the next poll where this one stopped.
pub(super) const TABLE_BUDGET_CAP: Duration = Duration::from_secs(150);

/// The longest a job used to wait for its device, whatever its interval; now the floor of that wait.
///
/// 60 s comfortably covers the slowest *healthy* walk measured in the lab (6.0 s against a
/// 232-interface switch) while keeping a long-interval check from parking behind a wedged device.
const SINGLE_FLIGHT_FLOOR: Duration = Duration::from_secs(60);

/// What a walk may run past its own deadline: the deadline stops the next request, not the one
/// already sent, so a walk can overrun by one round trip and its retry. 15 s covers that at the 2 s
/// timeout with room left for the scalar GET that queues behind.
const SINGLE_FLIGHT_OVERRUN: Duration = Duration::from_secs(15);

/// The budget one table job (its numeric walk and its name walk together) may spend:
/// half the interval, between [`TABLE_BUDGET_FLOOR`] and [`TABLE_BUDGET_CAP`].
///
/// **Half, so the job ends well before its successor is due** — the other half is for the node's
/// other specs, which queue behind it on the same device.
pub(super) fn table_job_budget(interval_secs: u32) -> Duration {
    if interval_secs == 0 {
        return TABLE_BUDGET_FLOOR;
    }
    (Duration::from_secs(u64::from(interval_secs)) / 2).clamp(TABLE_BUDGET_FLOOR, TABLE_BUDGET_CAP)
}

/// When each of a table job's two walks must stop.
///
/// 🚨 **One budget for the job, not one per walk.** Before Increment 10 each walk built its own, so a
/// job could spend twice what either promised. The numeric walk gets the first three quarters —
/// it carries the metric columns, and the interface rows that keep the ports from going stale — and
/// the name walk (`ifName` / `ifAlias`, two columns) ends at the budget. A numeric walk that finishes
/// early leaves the name walk everything it did not use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TableDeadlines {
    pub(super) numeric: Instant,
    pub(super) names: Instant,
}

impl TableDeadlines {
    /// The deadlines for a job that started at `started` and may spend `budget`.
    pub(super) fn from(started: Instant, budget: Duration) -> Self {
        Self {
            numeric: started + budget * 3 / 4,
            names: started + budget,
        }
    }
}

/// How long a job may wait for another probe against the same device to finish.
///
/// `min(interval, max(60 s, budget + 15 s))`, and 60 s for interval 0.
///
/// - **Bounded by the job's own interval**: a poll still waiting when its successor is due has
///   stopped being late and started being a queue, and shedding it is the honest answer.
/// - **At least long enough for a table job to finish** — its budget and one overrun. Before
///   Increment 10 this was capped at 60 s, which a 150 s walk at a 300 s interval would outlast,
///   shedding every spec queued behind it.
/// - **Unchanged at an interval of 90 s or less**, where the budget plus the overrun is under 60 s:
///   the fleet's usual intervals wait exactly what they did.
///
/// ⚠️ The budget is this job's interval's, not the one of the job ahead of it. A node's specs
/// normally share an interval; an hourly adjacency walk ahead of a one-minute job keeps its 16 s
/// budget (its walks name no deadline), which the 60 s floor already covers.
///
/// Zero-interval jobs (an operator's "poll now") get the floor rather than no wait at all — an
/// on-demand poll landing while the scheduled one is mid-walk should queue behind it, not report a
/// skip to the person who pressed the button.
pub(super) fn single_flight_wait(interval_secs: u32) -> Duration {
    if interval_secs == 0 {
        return SINGLE_FLIGHT_FLOOR;
    }
    let interval = Duration::from_secs(u64::from(interval_secs));
    interval.min(SINGLE_FLIGHT_FLOOR.max(table_job_budget(interval_secs) + SINGLE_FLIGHT_OVERRUN))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    /// **Half the interval, between the floor and the cap** — and the one-minute poll the PoC runs at
    /// gets 30 s, which is what GW01 needs to finish.
    #[test]
    fn the_table_budget_is_half_the_interval_between_the_floor_and_the_cap() {
        assert_eq!(table_job_budget(60), secs(30));
        assert_eq!(table_job_budget(120), secs(60));
        assert_eq!(table_job_budget(300), secs(150));
        assert_eq!(table_job_budget(30), secs(16), "below the floor");
        assert_eq!(table_job_budget(32), secs(16), "exactly the floor");
        assert_eq!(table_job_budget(34), secs(17), "just above it");
        assert_eq!(table_job_budget(3_600), secs(150), "the cap");
        assert_eq!(table_job_budget(u32::MAX), secs(150));
        assert_eq!(
            table_job_budget(0),
            TABLE_BUDGET_FLOOR,
            "poll now carries interval 0"
        );
    }

    /// The numeric walk ends at three quarters of the budget and the name walk at all of it, both
    /// counted from the same start.
    #[test]
    fn the_numeric_walk_gets_three_quarters_and_the_name_walk_ends_at_the_budget() {
        let started = Instant::now();
        let at_minute = TableDeadlines::from(started, table_job_budget(60));
        assert_eq!(at_minute.numeric - started, Duration::from_millis(22_500));
        assert_eq!(at_minute.names - started, secs(30));

        let at_floor = TableDeadlines::from(started, TABLE_BUDGET_FLOOR);
        assert_eq!(at_floor.numeric - started, secs(12));
        assert_eq!(at_floor.names - started, secs(16));
    }

    /// 🚨 **The accepting side of the wait: the intervals the fleet runs at wait what they did.**
    /// Before Increment 10 the wait was `min(interval, 60 s)`; at 90 s or less that is still the
    /// answer, so nothing that works today starts shedding or waiting longer.
    #[test]
    fn the_default_interval_keeps_todays_single_flight_wait() {
        for interval in [1u32, 10, 30, 45, 60, 90] {
            let before = secs(u64::from(interval)).min(secs(60));
            assert_eq!(
                single_flight_wait(interval),
                before,
                "interval {interval}s must keep its wait"
            );
        }
        assert_eq!(single_flight_wait(0), secs(60), "poll now keeps its wait");
    }

    /// A longer interval waits long enough for the table job ahead of it — its budget and one
    /// overrun — and never longer than its own interval.
    #[test]
    fn a_long_interval_waits_out_the_table_job_ahead_of_it() {
        assert_eq!(single_flight_wait(120), secs(75));
        assert_eq!(single_flight_wait(300), secs(165));
        assert_eq!(single_flight_wait(3_600), secs(165));
        for interval in [91u32, 100, 150, 200, 300, 600, 3_600] {
            let wait = single_flight_wait(interval);
            assert!(wait <= secs(u64::from(interval)));
            assert!(
                wait >= table_job_budget(interval) + SINGLE_FLIGHT_OVERRUN
                    || wait == secs(u64::from(interval)),
                "interval {interval}s waits {wait:?}, shorter than the job it waits behind"
            );
        }
    }
}
