// SPDX-License-Identifier: AGPL-3.0-only
//! A wall time in a named zone, turned into one instant (ADR-221).
//!
//! A preset schedule says "09:00 in Asia/Tokyo" rather than an instant, so every next-run
//! calculation has to ask the zone what 09:00 means on a given date. Two dates a year in a
//! daylight-saving zone have no single answer, and the rule for both lives here, once:
//!
//! - **A gap** (the clock jumps forward, so 02:30 never happens): the first instant after the gap.
//! - **An overlap** (the clock goes back, so 01:30 happens twice): the first of the two.
//!
//! Together with "strictly after now" at every caller, that means a schedule neither skips a day
//! nor fires twice on one (decision 5).
//!
//! It lives under core's database layer, not in `yagra-common`, because only core computes
//! schedules: the poller links `yagra-common`, and it has no reason to carry the zone database.

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
/// Re-exported so core names the zone type without a dependency of its own on `chrono-tz`.
pub use chrono_tz::Tz;

/// The zone a stored schedule names. `None` — a row written before ADR-221 — means UTC, which is
/// what those rows always meant. A name this build does not know (a newer zone database wrote it)
/// also falls to UTC rather than stopping the schedule.
#[must_use]
pub fn zone_or_utc(name: Option<&str>) -> Tz {
    name.and_then(parse_zone).unwrap_or(Tz::UTC)
}

/// An IANA zone name, or `None` when this build does not know it. The API edge refuses an unknown
/// name with a 400, so only a stored row can reach [`zone_or_utc`]'s fallback.
#[must_use]
pub fn parse_zone(name: &str) -> Option<Tz> {
    name.parse::<Tz>().ok()
}

/// The instant `hour:minute` on `date` means in `tz`, resolving a gap and an overlap as the module
/// doc says.
#[must_use]
pub fn resolve(tz: Tz, date: NaiveDate, hour: u32, minute: u32) -> DateTime<Utc> {
    let naive = date
        .and_hms_opt(hour.min(23), minute.min(59), 0)
        .unwrap_or_else(|| date.and_time(chrono::NaiveTime::MIN));
    match first_valid(tz, naive) {
        Some(t) => t,
        // No zone has a gap longer than a day; this is unreachable, and UTC is the safe answer.
        None => Utc.from_utc_datetime(&naive),
    }
}

/// The local date of `now` in `tz` — the "today" a schedule's calendar walk starts from.
#[must_use]
pub fn local_date(tz: Tz, now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&tz).date_naive()
}

fn first_valid(tz: Tz, naive: NaiveDateTime) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(t) => Some(t.with_timezone(&Utc)),
        LocalResult::Ambiguous(first, _) => Some(first.with_timezone(&Utc)),
        // Inside a gap: walk forward a minute at a time to the first wall time that exists, which
        // is the instant the gap ends. Gaps are whole minutes in every zone in the database.
        LocalResult::None => (1..=24 * 60).find_map(|m| {
            match tz.from_local_datetime(&(naive + Duration::minutes(m))) {
                LocalResult::Single(t) => Some(t.with_timezone(&Utc)),
                LocalResult::Ambiguous(first, _) => Some(first.with_timezone(&Utc)),
                LocalResult::None => None,
            }
        }),
    }
}

/// Every zone name this build's zone database knows, sorted - the names a person may choose from
/// anywhere in Yagra (ADR-253 decision 2).
///
/// The same set NetBox offers: its `TimeZoneField` lists `zoneinfo.available_timezones()` minus
/// `Factory`, which keeps the old aliases (`Japan`, `US/Eastern`) beside the current names. A
/// name is offered, stored and shown in the spelling it arrives in; nothing here maps an alias to
/// another name (decision 1).
#[must_use]
pub fn zone_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = chrono_tz::TZ_VARIANTS
        .iter()
        .map(|tz| tz.name())
        .filter(|name| *name != "Factory")
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
        .map(|name| format!("  {}", serde_json::Value::from(*name)))
        .collect();
    format!("[\n{}\n]\n", lines.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            assert!(names.contains(&name), "{name} is missing");
        }
        assert!(!names.contains(&"Factory"));
    }

    /// A name keeps the spelling it arrived in (ADR-253 decision 1): an alias is not turned into
    /// the zone it links to, though both keep the same time.
    #[test]
    fn a_zone_keeps_the_spelling_it_was_given() {
        for name in ["Japan", "US/Eastern", "Asia/Calcutta", "Asia/Tokyo"] {
            assert_eq!(parse_zone(name).map(|tz| tz.name()), Some(name));
        }
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 5, 0, 0).unwrap();
        let tokyo = parse_zone("Asia/Tokyo").unwrap();
        let japan = parse_zone("Japan").unwrap();
        assert_eq!(
            now.with_timezone(&tokyo).naive_local(),
            now.with_timezone(&japan).naive_local()
        );
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).expect("date")
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("ts")
            .with_timezone(&Utc)
    }

    #[test]
    fn an_ordinary_wall_time_is_its_one_instant() {
        assert_eq!(
            resolve(Tz::Asia__Tokyo, d(2026, 11, 1), 7, 0),
            utc("2026-10-31T22:00:00Z")
        );
    }

    #[test]
    fn a_wall_time_inside_the_spring_gap_resolves_to_the_end_of_the_gap() {
        // New York jumps 02:00 → 03:00 EDT on 2027-03-14; 02:30 does not exist.
        assert_eq!(
            resolve(Tz::America__New_York, d(2027, 3, 14), 2, 30),
            utc("2027-03-14T07:00:00Z")
        );
    }

    #[test]
    fn a_wall_time_inside_the_autumn_overlap_resolves_to_its_first_occurrence() {
        // 2026-11-01 01:30 happens at 05:30Z (EDT) and again at 06:30Z (EST).
        assert_eq!(
            resolve(Tz::America__New_York, d(2026, 11, 1), 1, 30),
            utc("2026-11-01T05:30:00Z")
        );
    }

    #[test]
    fn no_zone_means_utc_and_so_does_a_name_this_build_does_not_know() {
        assert_eq!(zone_or_utc(None), Tz::UTC);
        assert_eq!(zone_or_utc(Some("Mars/Olympus_Mons")), Tz::UTC);
        assert_eq!(zone_or_utc(Some("Asia/Tokyo")), Tz::Asia__Tokyo);
        assert!(parse_zone("Mars/Olympus_Mons").is_none());
    }
}
