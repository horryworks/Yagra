// SPDX-License-Identifier: AGPL-3.0-only
//! "Now", as the workspace spells it (ADR-184).
//!
//! Twenty-odd files computed the wall clock as Unix milliseconds or seconds by hand, and the copies
//! disagreed on the one case that matters — a clock before 1970 — between `0`, `i64::MAX` and a
//! panic (`.expect("clock")`). The answer here is `0`: a timestamp that is obviously wrong beats a
//! process that stops, and no copy's caller could do anything useful with a panic.
//!
//! [`monotonic_ms`] is for a rate limiter or a timeout: it never goes backwards when an operator
//! sets the clock, so a bucket cannot refill (or starve) because NTP stepped.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// The wall clock as Unix milliseconds; `0` before 1970, saturating far in the future.
#[must_use]
pub fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The wall clock as Unix seconds; `0` before 1970.
#[must_use]
pub fn now_unix_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The wall clock as unsigned Unix seconds; `0` before 1970.
#[must_use]
pub fn now_unix_s_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Milliseconds since this process first asked, on a clock that never goes backwards.
#[must_use]
pub fn monotonic_ms() -> i64 {
    static START: OnceLock<Instant> = OnceLock::new();
    let start = *START.get_or_init(Instant::now);
    i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// The sample step, in seconds, that keeps `[from_s, to_s]` under `max_points` samples — never
/// finer than 60 s, the shortest interval anything is collected at. Troubleshoot and Reports each
/// pass their own ceiling; the arithmetic is the same.
#[must_use]
pub fn read_step(from_s: i64, to_s: i64, max_points: i64) -> u64 {
    let span = (to_s - from_s).max(1);
    u64::try_from((span / max_points.max(1)).max(60)).unwrap_or(60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_is_read_in_at_most_max_points_steps_and_never_finer_than_a_minute() {
        assert_eq!(read_step(0, 3600, 300), 60, "an hour is read per minute");
        assert_eq!(read_step(0, 30 * 86_400, 300), 8640);
        assert_eq!(read_step(0, 30 * 86_400, 240), 10_800);
        assert_eq!(
            read_step(10, 0, 300),
            60,
            "a reversed window is not a panic"
        );
    }

    #[test]
    fn the_three_spellings_of_now_agree() {
        let ms = now_unix_ms();
        let s = now_unix_s();
        assert!(ms > 1_700_000_000_000, "the clock reads after 2023");
        assert!((ms / 1000 - s).abs() <= 1);
        assert_eq!(u64::try_from(s).unwrap() / 10, now_unix_s_u64() / 10);
    }

    #[test]
    fn the_monotonic_clock_does_not_go_backwards() {
        let a = monotonic_ms();
        let b = monotonic_ms();
        assert!(a >= 0 && b >= a);
    }
}
