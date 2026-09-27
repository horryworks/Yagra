// SPDX-License-Identifier: AGPL-3.0-only
//! Reading a setting from the environment, one way per shape (ADR-184).
//!
//! Every binary read its variables with the same four idioms spelled out by hand — "unset or blank
//! means unset", "a positive number or the default", "a comma list", "a yes/no switch" — and the
//! copies had drifted on whitespace (some trimmed, some did not, so `YAGRA_VM_WRITERS=" 4"` was
//! honoured by one reader and ignored by the next) and on what an unparseable number means.
//!
//! Each reader here is a thin wrapper over a pure `parse_*` that takes the raw text, so the rule
//! is testable without touching the process environment.
//!
//! ⚠️ **A secret is not read through here.** [`nonempty`] trims, and a password may legitimately
//! end in a space. Read one with `std::env::var` and keep it byte-exact.

use std::path::PathBuf;
use std::str::FromStr;

fn raw(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// Trimmed text; `None` when absent, empty or only whitespace.
#[must_use]
pub fn parse_nonempty(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A number greater than zero, or `default` — for unset, blank, unparseable, zero or negative.
#[must_use]
pub fn parse_positive<T: FromStr + PartialOrd + Default>(raw: Option<&str>, default: T) -> T {
    raw.and_then(|s| s.trim().parse::<T>().ok())
        .filter(|n| *n > T::default())
        .unwrap_or(default)
}

/// A comma-separated list, each entry trimmed, empty entries dropped.
#[must_use]
pub fn parse_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A switch: `1`/`true`/`yes`/`on` and `0`/`false`/`no`/`off`, case-insensitive and trimmed.
/// Anything else — unset, empty, a typo — is `default`.
///
/// ⚠️ **Empty must land on the default, not on `false`.** Compose renders `${VAR:-}` as an empty
/// string rather than omitting the variable, so a deployment that never chose hands over `""`, and
/// reading that as "the operator said no" would make an opt-out default unreachable through the
/// very file that ships it.
#[must_use]
pub fn parse_bool_or(raw: Option<&str>, default: bool) -> bool {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("1" | "true" | "yes" | "on") => true,
        Some("0" | "false" | "no" | "off") => false,
        _ => default,
    }
}

/// [`parse_nonempty`] over the variable `key`.
#[must_use]
pub fn nonempty(key: &str) -> Option<String> {
    parse_nonempty(raw(key).as_deref())
}

/// A path; `None` when unset or blank.
#[must_use]
pub fn path(key: &str) -> Option<PathBuf> {
    nonempty(key).map(PathBuf::from)
}

/// [`parse_positive`] over the variable `key`.
#[must_use]
pub fn positive<T: FromStr + PartialOrd + Default>(key: &str, default: T) -> T {
    parse_positive(raw(key).as_deref(), default)
}

/// [`parse_list`] over the variable `key`.
#[must_use]
pub fn list(key: &str) -> Vec<String> {
    parse_list(raw(key).as_deref())
}

/// [`parse_bool_or`] over the variable `key`.
#[must_use]
pub fn bool_or(key: &str, default: bool) -> bool {
    parse_bool_or(raw(key).as_deref(), default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_is_unset_and_the_value_is_trimmed() {
        assert_eq!(parse_nonempty(None), None);
        assert_eq!(parse_nonempty(Some("")), None);
        assert_eq!(parse_nonempty(Some("  \t")), None);
        assert_eq!(
            parse_nonempty(Some(" redis://r:6379 ")).as_deref(),
            Some("redis://r:6379")
        );
    }

    #[test]
    fn a_positive_number_is_honoured_and_anything_else_is_the_default() {
        assert_eq!(parse_positive(Some(" 4 "), 1usize), 4);
        assert_eq!(parse_positive(Some("0"), 7usize), 7);
        assert_eq!(parse_positive(Some("-3"), 7i64), 7);
        assert_eq!(parse_positive(Some("four"), 7u32), 7);
        assert_eq!(parse_positive(None, 7u32), 7);
        assert!((parse_positive(Some("2.5"), 1.0f64) - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn a_list_drops_blanks_and_trims() {
        assert_eq!(parse_list(Some(" a, ,b ,,c")), vec!["a", "b", "c"]);
        assert!(parse_list(None).is_empty());
        assert!(parse_list(Some("")).is_empty());
    }

    #[test]
    fn a_switch_knows_both_vocabularies_and_defaults_on_everything_else() {
        for yes in ["1", "true", "YES", " on "] {
            assert!(parse_bool_or(Some(yes), false), "{yes}");
        }
        for no in ["0", "False", "no", "OFF"] {
            assert!(!parse_bool_or(Some(no), true), "{no}");
        }
        assert!(
            parse_bool_or(Some(""), true),
            "empty is the default, not no"
        );
        assert!(parse_bool_or(Some("maybe"), true));
        assert!(!parse_bool_or(None, false));
    }
}
