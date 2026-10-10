// SPDX-License-Identifier: AGPL-3.0-only
//! Cleaning device-supplied text before it is stored or shown (ADR-202). One loop, shared by every
//! kind of value that needs it — [`crate::serial::sanitize`] and [`crate::os_version::sanitize`]
//! differ only in their cap.

/// Control characters and runs of whitespace become one space, the ends are trimmed, and the
/// result is cut at `max_chars` characters — at a character boundary, never inside a gap. `None`
/// when nothing is left.
#[must_use]
pub fn fold_and_cap(raw: &str, max_chars: usize) -> Option<String> {
    let mut out = String::new();
    let mut count = 0usize;
    let mut gap = false;
    for c in raw.chars() {
        if c.is_whitespace() || c.is_control() {
            gap = count > 0;
            continue;
        }
        let needed = if gap { 2 } else { 1 };
        if count + needed > max_chars {
            break;
        }
        if gap {
            out.push(' ');
            count += 1;
            gap = false;
        }
        out.push(c);
        count += 1;
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::fold_and_cap;

    #[test]
    fn folds_trims_and_caps_on_a_character_boundary() {
        assert_eq!(
            fold_and_cap("  a\u{0}b \t c ", 16).as_deref(),
            Some("a b c")
        );
        assert_eq!(fold_and_cap(" \r\n ", 16), None);
        // A gap is never left dangling at the cut: "ab cd" capped at 3 is "ab", not "ab ".
        assert_eq!(fold_and_cap("ab cd", 3).as_deref(), Some("ab"));
        assert_eq!(fold_and_cap("ééé", 2).as_deref(), Some("éé"));
    }
}
