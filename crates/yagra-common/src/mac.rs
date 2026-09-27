// SPDX-License-Identifier: AGPL-3.0-only
//! A MAC address, read and written one way (ADR-184).
//!
//! Before this module the same six-octet parser lived in `yagra-oui` and again, privately, in
//! `yagra-transport`'s Meraki neighbour reader — one of them trimmed its input and the other did
//! not. The rendering rule (lowercase, colon-separated) already lived in `neighbor.rs`; it moves
//! here so the pair sits together, and `neighbor.rs` re-exports it.

/// Parse a MAC written as six hex octets separated by `:` or `-`, in either case, with surrounding
/// whitespace ignored. `None` for anything else — a dotted Cisco form, twelve bare digits or a text
/// id that merely looks like a MAC is the caller's to handle first.
#[must_use]
pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut parts = s.trim().split([':', '-']);
    for slot in &mut out {
        let part = parts.next()?;
        if part.len() != 2 {
            return None;
        }
        *slot = u8::from_str_radix(part, 16).ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// Six octets as lowercase colon-separated hex. `None` for any other length — that is not a MAC,
/// whatever the subtype claimed.
#[must_use]
pub fn render_mac(bytes: &[u8]) -> Option<String> {
    let mac: &[u8; 6] = bytes.try_into().ok()?;
    Some(
        mac.iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mac_accepts_both_separators_either_case_and_surrounding_space_and_nothing_else() {
        let want = Some([0x00, 0x1b, 0x54, 0xff, 0x00, 0x9a]);
        assert_eq!(parse_mac("00:1b:54:ff:00:9a"), want);
        assert_eq!(parse_mac("00-1B-54-FF-00-9A"), want);
        assert_eq!(parse_mac(" 00:1b:54:ff:00:9a\n"), want);
        assert_eq!(parse_mac("001b.54ff.009a"), None);
        assert_eq!(parse_mac("00:1b:54:ff:00"), None);
        assert_eq!(parse_mac("00:1b:54:ff:00:9a:01"), None);
        assert_eq!(parse_mac("00:1b:54:ff:00:9g"), None);
        assert_eq!(parse_mac("0:1b:54:ff:00:9a"), None);
        assert_eq!(parse_mac(""), None);
    }

    #[test]
    fn a_parsed_mac_renders_back_in_the_one_spelling() {
        let mac = parse_mac("00-1B-54-FF-00-9A").unwrap();
        assert_eq!(render_mac(&mac).as_deref(), Some("00:1b:54:ff:00:9a"));
        assert_eq!(render_mac(&[1, 2, 3]), None);
    }
}
