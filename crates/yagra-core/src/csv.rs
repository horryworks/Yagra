// SPDX-License-Identifier: AGPL-3.0-only
//! CSV encoding for every file this core hands an operator.
//!
//! **One encoder, because a duplicated encoder is a duplicated security boundary.** The WebUI
//! learned that the expensive way: two byte-identical field encoders existed in TypeScript and
//! *neither* neutralized spreadsheet formulas, so the audit log — which stores the username
//! submitted to a **failed** sign-in — could carry `=HYPERLINK(…)` planted by anyone who could
//! reach the sign-in page, and it would run for the admin who exported the log. The fix had to land
//! in one place to be checkable at all (`extensibility.md` §3).
//!
//! This file is the Rust half of that rule, and it is a **mirror** of `web/src/lib/csv.ts`
//! (`extensibility.md` §2). The two must agree on what they neutralize and how they quote; the test
//! at the bottom pins the cases the TypeScript side pins, so a change to one that is not made to
//! the other fails here rather than in an exported file nobody re-reads.
//!
//! Report exports went out without this for their whole life. A report table's cells are device-
//! supplied — node names, `ifAlias`, `sysDescr` — so the same hole existed there, quieter.

use std::fmt::Write as _;

/// Characters a spreadsheet treats as the start of a formula.
///
/// `\t` and `\r` are here because a leading one of them lets the next character take that role
/// after the cell is parsed. The same six as the TypeScript side, in the same order.
pub const FORMULA_TRIGGERS: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// Whether a value would be read as a formula, after the numeric exemption.
///
/// A negative number is exempt: `-5` is data an operator expects to see as a number, and quoting it
/// as text would corrupt every numeric column. The exemption is anchored at **both** ends, so a
/// payload that merely *starts* like a number (`-5+cmd|' /C calc'!A0`) fails it and is neutralized.
/// Only `-` gets an exemption, because it is the only trigger a stringified number can begin with.
fn is_formula_like(s: &str) -> bool {
    let Some(first) = s.chars().next() else {
        return false;
    };
    if !FORMULA_TRIGGERS.contains(&first) {
        return false;
    }
    !is_negative_number(s)
}

/// `-` followed by a plain decimal, optionally with an exponent, and **nothing else**.
///
/// Hand-written rather than a regex because the whole point is the anchoring, and a regex crate
/// dependency for six lines is not worth the second thing to keep honest.
fn is_negative_number(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('-') else {
        return false;
    };
    let (mantissa, exponent) = match rest.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (rest, None),
    };
    // `12`, `12.`, `12.5` or `.5` — digits on at least one side of an optional single point.
    let (int, frac) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mantissa, ""),
    };
    if int.is_empty() && frac.is_empty() {
        return false;
    }
    if !int.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    match exponent {
        None => true,
        Some(e) => {
            let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
            !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
        }
    }
}

/// Encode one CSV field: neutralize a formula-triggering value with a leading apostrophe, then
/// quote per RFC 4180.
///
/// Quoting is **unconditional** rather than "quote when it contains a comma". That is not
/// cosmetic: deciding per value is a rule that can be got wrong once and corrupt every row after
/// it, and the previous Rust encoder decided per value. Output for a benign value differs from the
/// old one only by the quotes, which every reader strips.
///
/// The apostrophe goes *inside* the quotes because that is where the cell's text begins — a
/// spreadsheet consumes it as the "treat this as literal text" marker and displays the value
/// unchanged.
#[must_use]
pub fn field(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    if is_formula_like(s) {
        out.push('\'');
    }
    for c in s.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Join fields into one CSV record.
#[must_use]
pub fn row(fields: &[&str]) -> String {
    let mut out = String::new();
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "{}", field(f));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_benign_value_is_quoted_and_otherwise_untouched() {
        assert_eq!(field("plain"), "\"plain\"");
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field(""), "\"\"");
        assert_eq!(field("with\nnewline"), "\"with\nnewline\"");
    }

    #[test]
    fn an_embedded_quote_is_doubled() {
        assert_eq!(field("he\"llo"), "\"he\"\"llo\"");
        assert_eq!(field("\"\""), "\"\"\"\"\"\"");
    }

    #[test]
    fn every_formula_trigger_is_neutralized() {
        // The audit log stores the username submitted to a FAILED sign-in, so these are values an
        // unauthenticated stranger can choose. Each must reach the spreadsheet as text.
        for trigger in FORMULA_TRIGGERS {
            let payload = format!("{trigger}HYPERLINK(\"http://evil\",\"click\")");
            let encoded = field(&payload);
            assert!(
                encoded.starts_with("\"'"),
                "{trigger:?} was not neutralized: {encoded}"
            );
        }
    }

    #[test]
    fn a_negative_number_stays_a_number_but_a_payload_wearing_one_does_not() {
        // The exemption exists so numeric columns survive; it is anchored at both ends so it
        // cannot be used as a prefix to smuggle a formula through.
        for n in ["-5", "-0.87", "-.5", "-12.", "-1e10", "-1.5E-3"] {
            assert_eq!(field(n), format!("\"{n}\""), "{n} should stay a number");
        }
        for payload in [
            "-5+cmd|' /C calc'!A0",
            "-1;=WEBSERVICE(\"http://evil\")",
            "-5\n=HYPERLINK(\"http://evil\")",
            "-",
            "-1e",
            "-1e+",
        ] {
            assert!(
                field(payload).starts_with("\"'"),
                "{payload:?} should be neutralized"
            );
        }
    }

    #[test]
    fn the_rules_match_the_typescript_encoder_they_mirror() {
        // `web/src/lib/csv.ts` is the other half. Nothing but this compares them, and a
        // disagreement is an export that is safe on one surface and not on the other.
        let ts = include_str!("../../../web/src/lib/csv.ts");
        // Needles built from the constant, so this test cannot pass by matching itself.
        for trigger in FORMULA_TRIGGERS {
            let spelled = match trigger {
                '\t' => "'\\t'".to_owned(),
                '\r' => "'\\r'".to_owned(),
                c => format!("'{c}'"),
            };
            assert!(
                ts.contains(&spelled),
                "{trigger:?} is a trigger here but not in lib/csv.ts"
            );
        }
        // Both quote unconditionally. If the TypeScript side ever goes back to conditional
        // quoting, the two encoders produce different files for the same rows.
        assert!(ts.contains("Quoting is unconditional"));
    }

    #[test]
    fn a_row_separates_fields_with_a_comma_and_quotes_each() {
        assert_eq!(row(&["a", "b"]), "\"a\",\"b\"");
        assert_eq!(row(&[]), "");
        assert_eq!(row(&["=1"]), "\"'=1\"");
    }
}
