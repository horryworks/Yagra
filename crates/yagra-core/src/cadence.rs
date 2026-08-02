// SPDX-License-Identifier: AGPL-3.0-only
//! Preset firing cadence, shared by every kind of schedule.
//!
//! Two things fire on a recurring preset today — a report definition and a Troubleshoot analysis —
//! and both need the same answer to "when does this next run". [`compute_next_run`] is that answer,
//! written once.
//!
//! It was tempting to copy it: the second caller needs the identical daily/weekly/monthly walk with
//! nothing added. That is exactly the shape `extensibility.md` §3 warns about — the month-end and
//! DST branches are where the subtlety is, and a divergent copy of *those* produces a schedule that
//! fires on the wrong day once a year, in one of the two features, with nothing failing anywhere.
//!
//! Deliberately preset rather than cron. The WebUI offers daily / weekly / monthly at a time of day;
//! a cron expression would need a parser, a validator and an explanation, and no one has asked for
//! one. Times are UTC throughout, so a schedule fires at the same instant regardless of the
//! database session's timezone.

use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use serde::Serialize;

use crate::stored_enum::token_enum;

/// How often a schedule fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Cadence {
    Daily,
    Weekly,
    Monthly,
    /// A cadence this build does not know — a newer core wrote it. Treated as daily when computing
    /// the next run, which is what the old wildcard arm did.
    Unknown,
}

token_enum!(Cadence, Unknown, "*_schedules.frequency", [
    Daily => "daily",
    Weekly => "weekly",
    Monthly => "monthly",
    Unknown => "unknown",
]);

/// The fields that decide when a preset schedule next fires.
///
/// A struct rather than five positional arguments because the two `Option<i16>` days and the two
/// `i16` times are trivially transposable at a call site, and the compiler cannot tell — a schedule
/// that fires at 30 minutes past hour 9 rather than 9:30 is a bug you find in production.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub frequency: Cadence,
    /// 0=Sun … 6=Sat, for [`Cadence::Weekly`].
    pub day_of_week: Option<i16>,
    /// 1 … 28, for [`Cadence::Monthly`] (clamped so every month has the day).
    pub day_of_month: Option<i16>,
    pub at_hour: i16,
    pub at_minute: i16,
}

/// Compute the next firing instant, strictly after `now`. Pure, so the month-end and roll-over
/// cases are unit-tested without a clock or a database.
#[must_use]
pub fn compute_next_run(s: Schedule, now: DateTime<Utc>) -> DateTime<Utc> {
    let hour = s.at_hour.clamp(0, 23) as u32;
    let minute = s.at_minute.clamp(0, 59) as u32;
    let at_time = |date: NaiveDate| -> DateTime<Utc> {
        let naive = date
            .and_hms_opt(hour, minute, 0)
            .unwrap_or_else(|| date.and_hms_opt(0, 0, 0).unwrap_or_else(|| now.naive_utc()));
        Utc.from_utc_datetime(&naive)
    };
    match s.frequency {
        Cadence::Weekly => {
            // 0=Sun..6=Sat; chrono's num_days_from_sunday matches.
            let target = i64::from(s.day_of_week.unwrap_or(0).clamp(0, 6));
            let current = i64::from(now.weekday().num_days_from_sunday());
            let mut days = (target - current).rem_euclid(7);
            let mut candidate = at_time(now.date_naive() + ChronoDuration::days(days));
            if candidate <= now {
                days += 7;
                candidate = at_time(now.date_naive() + ChronoDuration::days(days));
            }
            candidate
        }
        Cadence::Monthly => {
            // Clamp to 28 so every month has the day.
            let dom = s.day_of_month.unwrap_or(1).clamp(1, 28) as u32;
            let (mut y, mut m) = (now.year(), now.month());
            if let Some(date) = NaiveDate::from_ymd_opt(y, m, dom) {
                let candidate = at_time(date);
                if candidate > now {
                    return candidate;
                }
            }
            // Advance to next month.
            if m == 12 {
                y += 1;
                m = 1;
            } else {
                m += 1;
            }
            let date = NaiveDate::from_ymd_opt(y, m, dom)
                .unwrap_or_else(|| now.date_naive() + ChronoDuration::days(28));
            at_time(date)
        }
        // An Unknown cadence falls to daily, which is exactly what the old wildcard arm did —
        // named now, so a fifth cadence has to choose rather than inherit this by accident.
        Cadence::Daily | Cadence::Unknown => {
            let candidate = at_time(now.date_naive());
            if candidate > now {
                candidate
            } else {
                at_time(now.date_naive() + ChronoDuration::days(1))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("test timestamp")
            .with_timezone(&Utc)
    }

    fn sched(frequency: Cadence, at_hour: i16, at_minute: i16) -> Schedule {
        Schedule {
            frequency,
            day_of_week: None,
            day_of_month: None,
            at_hour,
            at_minute,
        }
    }

    #[test]
    fn daily_rolls_to_tomorrow_when_the_time_has_passed() {
        let now = at("2026-07-28T09:00:00Z");
        assert_eq!(
            compute_next_run(sched(Cadence::Daily, 8, 0), now),
            at("2026-07-29T08:00:00Z")
        );
        assert_eq!(
            compute_next_run(sched(Cadence::Daily, 10, 30), now),
            at("2026-07-28T10:30:00Z")
        );
    }

    #[test]
    fn weekly_lands_on_the_named_weekday_and_never_on_now() {
        // 2026-07-28 is a Tuesday. Asking for Tuesday at a time already past must go a week out,
        // not fire immediately — a schedule that returns `now` fires in a loop.
        let now = at("2026-07-28T09:00:00Z");
        let mut s = sched(Cadence::Weekly, 8, 0);
        s.day_of_week = Some(2); // Tue
        let next = compute_next_run(s, now);
        assert_eq!(next, at("2026-08-04T08:00:00Z"));
        assert!(next > now);
    }

    #[test]
    fn monthly_clamps_past_the_28th_so_february_still_fires() {
        // The reason day_of_month is capped at 28: a "31st" schedule would skip February entirely.
        let now = at("2026-01-15T00:00:00Z");
        let mut s = sched(Cadence::Monthly, 6, 0);
        s.day_of_month = Some(31);
        assert_eq!(compute_next_run(s, now), at("2026-01-28T06:00:00Z"));
        assert_eq!(
            compute_next_run(s, at("2026-01-29T00:00:00Z")),
            at("2026-02-28T06:00:00Z")
        );
    }

    #[test]
    fn monthly_rolls_over_the_year_boundary() {
        let mut s = sched(Cadence::Monthly, 0, 0);
        s.day_of_month = Some(5);
        assert_eq!(
            compute_next_run(s, at("2026-12-20T00:00:00Z")),
            at("2027-01-05T00:00:00Z")
        );
    }

    #[test]
    fn an_unknown_cadence_falls_back_to_daily_rather_than_never_firing() {
        // A schedule written by a newer core must still fire. Never firing would look like the
        // feature is broken; firing daily is visibly wrong in a way an operator can act on.
        let now = at("2026-07-28T09:00:00Z");
        assert_eq!(
            compute_next_run(sched(Cadence::Unknown, 8, 0), now),
            compute_next_run(sched(Cadence::Daily, 8, 0), now)
        );
    }

    #[test]
    fn an_out_of_range_time_is_clamped_rather_than_panicking() {
        // 99:99 clamps to 23:59, which is still ahead of 09:00 — so it fires today. Naming both
        // sides: the clamp happens, and it does not also push the firing out a day.
        let now = at("2026-07-28T09:00:00Z");
        assert_eq!(
            compute_next_run(sched(Cadence::Daily, 99, 99), now),
            at("2026-07-28T23:59:00Z")
        );
        // Negative clamps to 00:00, which is behind 09:00 — so that one does roll to tomorrow.
        assert_eq!(
            compute_next_run(sched(Cadence::Daily, -5, -5), now),
            at("2026-07-29T00:00:00Z")
        );
    }

    #[test]
    fn token_and_serde_agree_for_every_cadence() {
        // The column value and the JSON tag are produced by two different mechanisms — `as_str`
        // and `#[serde(rename_all)]` — and a disagreement means rows the writer produces are rows
        // the reader cannot parse.
        for c in Cadence::ALL {
            let json = serde_json::to_string(c).expect("cadence serializes");
            assert_eq!(json, format!("\"{}\"", c.as_str()));
            assert_eq!(Cadence::from_stored(c.as_str()), *c);
        }
        assert_eq!(Cadence::from_stored("fortnightly"), Cadence::Unknown);
    }
}
