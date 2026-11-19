// SPDX-License-Identifier: AGPL-3.0-only
//! A wall time in a named zone, turned into one instant (ADR-221).
//!
//! A preset schedule says "09:00 in Asia/Tokyo" rather than an instant, so every next-run
//! calculation has to ask the zone what 09:00 means on a given date. Two dates a year in a
//! daylight-saving zone have no single answer, and the rule for both is `jiff`'s "compatible"
//! one (ADR-221 Inc.3 decision 9), the same as RFC 5545 and JavaScript's Temporal:
//!
//! - **A gap** (the clock jumps forward, so 02:30 never happens): read with the offset in force
//!   before the gap, so 02:30 becomes 03:30.
//! - **An overlap** (the clock goes back, so 01:30 happens twice): the first of the two.
//!
//! Together with "strictly after now" at every caller, that means a schedule neither skips a day
//! nor fires twice on one (decision 5).
//!
//! `jiff` is named here and nowhere else (decision 12): everything outside this module speaks
//! `chrono`, and `Tz` is the one `jiff` type it sees. The zone database is compiled into the
//! binary (decision 11), so every build answers from the same rules wherever it runs.
//!
//! It lives under core's database layer, not in `yagra-common`, because only core computes
//! schedules: the poller links `yagra-common`, and it has no reason to carry the zone database.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};

/// A named zone. `jiff`'s type, so it is `Clone` rather than `Copy`, and two names for the same
/// rules (`Japan` and `Asia/Tokyo`) compare equal.
pub type Tz = jiff::tz::TimeZone;

/// The zone a stored schedule names. `None` — a row written before ADR-221 — means UTC, which is
/// what those rows always meant. A name this build does not know (a newer zone database wrote it)
/// also falls to UTC rather than stopping the schedule.
#[must_use]
pub fn zone_or_utc(name: Option<&str>) -> Tz {
    name.and_then(parse_zone).unwrap_or(Tz::UTC)
}

/// An IANA zone name, or `None` when this build does not know it. The API edge refuses an unknown
/// name with a 400, so only a stored row can reach [`zone_or_utc`]'s fallback. Letter case is not
/// significant (decision 10): `asia/tokyo` is `Asia/Tokyo`, and [`zone_name`] gives the spelling
/// the database uses.
#[must_use]
pub fn parse_zone(name: &str) -> Option<Tz> {
    Tz::get(name).ok()
}

/// The name a zone is stored and shown under: the database's own spelling of the name it was
/// looked up by. An alias keeps its own name (ADR-253 decision 1).
#[must_use]
pub fn zone_name(tz: &Tz) -> &str {
    tz.iana_name().unwrap_or("UTC")
}

/// The instant `hour:minute` on `date` means in `tz`, resolving a gap and an overlap as the module
/// doc says.
#[must_use]
pub fn resolve(tz: &Tz, date: NaiveDate, hour: u32, minute: u32) -> DateTime<Utc> {
    let naive = date
        .and_hms_opt(hour.min(23), minute.min(59), 0)
        .unwrap_or_else(|| date.and_time(chrono::NaiveTime::MIN));
    civil(naive)
        // `to_timestamp` resolves with jiff's "compatible" rule, the one the module doc states.
        .and_then(|dt| tz.to_timestamp(dt).ok())
        .and_then(from_timestamp)
        // Only a date outside jiff's range gets here (years beyond +/-9999); UTC is the safe answer.
        .unwrap_or_else(|| naive.and_utc())
}

/// The wall clock `now` shows in `tz`.
#[must_use]
pub fn to_local(tz: &Tz, now: DateTime<Utc>) -> NaiveDateTime {
    let Some(local) = timestamp(now).map(|ts| tz.to_datetime(ts)) else {
        return now.naive_utc();
    };
    NaiveDate::from_ymd_opt(
        i32::from(local.year()),
        u32::try_from(local.month()).unwrap_or(1),
        u32::try_from(local.day()).unwrap_or(1),
    )
    .and_then(|d| {
        d.and_hms_opt(
            u32::try_from(local.hour()).unwrap_or(0),
            u32::try_from(local.minute()).unwrap_or(0),
            u32::try_from(local.second()).unwrap_or(0),
        )
    })
    .unwrap_or_else(|| now.naive_utc())
}

/// The local date of `now` in `tz` — the "today" a schedule's calendar walk starts from.
#[must_use]
pub fn local_date(tz: &Tz, now: DateTime<Utc>) -> NaiveDate {
    to_local(tz, now).date()
}

fn civil(naive: NaiveDateTime) -> Option<jiff::civil::DateTime> {
    use chrono::{Datelike, Timelike};
    jiff::civil::DateTime::new(
        i16::try_from(naive.year()).ok()?,
        i8::try_from(naive.month()).ok()?,
        i8::try_from(naive.day()).ok()?,
        i8::try_from(naive.hour()).ok()?,
        i8::try_from(naive.minute()).ok()?,
        i8::try_from(naive.second()).ok()?,
        0,
    )
    .ok()
}

fn timestamp(t: DateTime<Utc>) -> Option<jiff::Timestamp> {
    let nanos = i32::try_from(t.timestamp_subsec_nanos()).ok()?;
    jiff::Timestamp::new(t.timestamp(), nanos).ok()
}

fn from_timestamp(ts: jiff::Timestamp) -> Option<DateTime<Utc>> {
    let nanos = u32::try_from(ts.subsec_nanosecond()).ok()?;
    DateTime::from_timestamp(ts.as_second(), nanos)
}

/// Every zone name this build's zone database knows, sorted - the names a person may choose from
/// anywhere in Yagra (ADR-253 decision 2).
///
/// The same set NetBox offers: its `TimeZoneField` lists `zoneinfo.available_timezones()` minus
/// `Factory`, which keeps the old aliases (`Japan`, `US/Eastern`) beside the current names. A
/// name is offered, stored and shown in the spelling it arrives in; nothing here maps an alias to
/// another name (decision 1).
#[must_use]
pub fn zone_names() -> Vec<String> {
    let mut names: Vec<String> = jiff::tz::db()
        .available()
        .map(|name| name.to_string())
        .filter(|name| name != "Factory")
        .collect();
    names.sort_unstable();
    names
}

/// [`zone_names`] as the JSON array the WebUI reads (`web/src/lib/zoneNames.json`), one name a
/// line so a zone database update reads as a short diff.
#[must_use]
pub fn zone_names_json() -> String {
    let lines: Vec<String> = zone_names()
        .iter()
        .map(|name| format!("  {}", serde_json::Value::from(name.as_str())))
        .collect();
    format!("[\n{}\n]\n", lines.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// The WebUI's picker is generated from this crate, never transcribed (ADR-253 decision 2).
    #[test]
    fn the_committed_zone_names_are_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../web/src/lib/zoneNames.json");
        let generated = zone_names_json();
        if std::env::var_os("UPDATE_ZONE_NAMES").is_some() {
            std::fs::write(&path, &generated).expect("write zoneNames.json");
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed, generated,
            "web/src/lib/zoneNames.json is stale. Regenerate it with:\n    \
             UPDATE_ZONE_NAMES=1 cargo test -p yagra-base the_committed_zone_names_are_current"
        );
    }

    /// NetBox offers the aliases beside the current names and leaves out only `Factory`; so does
    /// this list. A list holding only current names would refuse a zone NetBox accepted.
    #[test]
    fn the_names_are_the_ones_netbox_offers() {
        let names = zone_names();
        assert!(
            names.len() > 500,
            "the zone database is much larger than this"
        );
        for name in [
            "Asia/Tokyo",
            "Japan",
            "US/Eastern",
            "Asia/Kolkata",
            "Asia/Calcutta",
            "UTC",
        ] {
            assert!(names.iter().any(|n| n == name), "{name} is missing");
        }
        assert!(!names.iter().any(|n| n == "Factory"));
    }

    /// A name keeps the spelling it arrived in (ADR-253 decision 1): an alias is not turned into
    /// the zone it links to, though both keep the same time.
    #[test]
    fn a_zone_keeps_the_spelling_it_was_given() {
        for name in ["Japan", "US/Eastern", "Asia/Calcutta", "Asia/Tokyo"] {
            assert_eq!(parse_zone(name).as_ref().map(zone_name), Some(name));
        }
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 5, 0, 0).unwrap();
        let tokyo = parse_zone("Asia/Tokyo").unwrap();
        let japan = parse_zone("Japan").unwrap();
        assert_eq!(to_local(&tokyo, now), to_local(&japan, now));
    }

    /// Letter case is not significant, and the name comes back in the database's spelling
    /// (decision 10), so what is stored matches the picker's list.
    #[test]
    fn a_name_in_the_wrong_case_is_read_and_spelled_the_databases_way() {
        let tz = parse_zone("asia/TOKYO").expect("case-insensitive lookup");
        assert_eq!(zone_name(&tz), "Asia/Tokyo");
        assert_eq!(zone_name(&Tz::UTC), "UTC");
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).expect("date")
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("ts")
            .with_timezone(&Utc)
    }

    fn zone(name: &str) -> Tz {
        parse_zone(name).expect("zone")
    }

    #[test]
    fn an_ordinary_wall_time_is_its_one_instant() {
        assert_eq!(
            resolve(&zone("Asia/Tokyo"), d(2026, 11, 1), 7, 0),
            utc("2026-10-31T22:00:00Z")
        );
    }

    #[test]
    fn a_wall_time_inside_the_spring_gap_is_read_with_the_offset_before_the_gap() {
        // New York jumps 02:00 -> 03:00 EDT on 2027-03-14; 02:30 does not exist and is read at
        // EST (-05:00), which is 03:30 EDT.
        assert_eq!(
            resolve(&zone("America/New_York"), d(2027, 3, 14), 2, 30),
            utc("2027-03-14T07:30:00Z")
        );
    }

    #[test]
    fn a_wall_time_inside_the_autumn_overlap_resolves_to_its_first_occurrence() {
        // 2026-11-01 01:30 happens at 05:30Z (EDT) and again at 06:30Z (EST).
        assert_eq!(
            resolve(&zone("America/New_York"), d(2026, 11, 1), 1, 30),
            utc("2026-11-01T05:30:00Z")
        );
    }

    #[test]
    fn the_local_wall_clock_and_date_follow_the_zone() {
        let now = utc("2026-10-31T22:30:15Z");
        assert_eq!(
            to_local(&zone("Asia/Tokyo"), now),
            d(2026, 11, 1).and_hms_opt(7, 30, 15).unwrap()
        );
        assert_eq!(local_date(&zone("Asia/Tokyo"), now), d(2026, 11, 1));
        assert_eq!(local_date(&Tz::UTC, now), d(2026, 10, 31));
    }

    #[test]
    fn no_zone_means_utc_and_so_does_a_name_this_build_does_not_know() {
        assert_eq!(zone_or_utc(None), Tz::UTC);
        assert_eq!(zone_or_utc(Some("Mars/Olympus_Mons")), Tz::UTC);
        assert_eq!(zone_name(&zone_or_utc(Some("Asia/Tokyo"))), "Asia/Tokyo");
        assert!(parse_zone("Mars/Olympus_Mons").is_none());
    }
}
