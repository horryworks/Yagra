// SPDX-License-Identifier: AGPL-3.0-only
//! A device's serial number, read out of ENTITY-MIB's chassis rows (ADR-147).
//!
//! Pure, like [`crate::os_version`]: the poller holds the SNMP session and walks the two columns
//! named here, and this module decides what the rows mean. Two questions, one function each:
//!
//!  - [`resolve`] — given the walked rows, the serial number to show, or `None`;
//!  - [`sanitize`] — the one cap every serial passes, applied on **both** sides of the bus.
//!
//! ## Why only the chassis rows
//!
//! ENTITY-MIB carries a serial for every part it lists — line cards, power supplies, transceivers —
//! and which row is "the device" differs per vendor: `1` on most, `1001`/`2001`/`3001` on a Catalyst
//! stack, `149` on NX-OS, `67108867` on a Huawei S5720. The class column says which rows are a
//! chassis, so nothing guesses an index. Measured on the lab's 22 recordings (ADR-147 decision 1):
//! 13 right and none wrong. The two rules this was chosen over each took a wrong value there — the
//! first non-empty row gave an ASR9010 a line card's serial and a RouterOS box the name of a USB
//! device, and index 1 is empty on both stacks.
//!
//! ## A stack shows every member
//!
//! Several chassis rows are joined in index order with `, ` (ADR-147 decision 2). A serial repeated
//! on two rows is listed once, at most [`SERIAL_MAX_CHASSIS`] are listed, and the list stops before
//! a serial that would take it past [`SERIAL_MAX_CHARS`] rather than cutting one in half.

use std::collections::BTreeMap;

/// `entPhysicalClass` — what kind of component an ENTITY-MIB row is.
pub const OID_ENT_PHYSICAL_CLASS: &str = "1.3.6.1.2.1.47.1.1.1.1.5";
/// `entPhysicalSerialNum` — the vendor's serial number for that component.
pub const OID_ENT_PHYSICAL_SERIAL_NUM: &str = "1.3.6.1.2.1.47.1.1.1.1.11";
/// `entPhysicalClass`'s `chassis(3)`.
pub const CLASS_CHASSIS: i64 = 3;
/// The longest serial — or list of a stack's serials — a node keeps. Longer is cut, never refused.
pub const SERIAL_MAX_CHARS: usize = 128;
/// The most chassis whose serials are listed for one node.
pub const SERIAL_MAX_CHASSIS: usize = 8;

/// What goes between two members' serials.
const SEPARATOR: &str = ", ";

/// The serial number to show for a device, from its ENTITY-MIB rows keyed by `entPhysicalIndex`:
/// every chassis row's serial, in index order, joined with `, `. `None` when no chassis row
/// carries one.
///
/// A serial with no class row beside it is not taken, and a chassis row with an empty serial is
/// skipped rather than listed as a blank — a RouterOS box reports exactly that, with its only
/// non-empty serial on a USB device.
#[must_use]
pub fn resolve(classes: &BTreeMap<u32, i64>, serials: &BTreeMap<u32, String>) -> Option<String> {
    let mut listed: Vec<String> = Vec::new();
    let mut chars = 0usize;
    for (index, class) in classes {
        if *class != CLASS_CHASSIS {
            continue;
        }
        let Some(serial) = serials.get(index).and_then(|raw| sanitize(raw)) else {
            continue;
        };
        if listed.contains(&serial) {
            continue;
        }
        let width = if listed.is_empty() {
            serial.chars().count()
        } else {
            SEPARATOR.len() + serial.chars().count()
        };
        if !listed.is_empty() && chars + width > SERIAL_MAX_CHARS {
            break;
        }
        chars += width;
        listed.push(serial);
        if listed.len() == SERIAL_MAX_CHASSIS {
            break;
        }
    }
    (!listed.is_empty()).then(|| listed.join(SEPARATOR))
}

/// Make a device-supplied serial safe to store and show: control characters and runs of whitespace
/// become one space, the ends are trimmed, and the result is cut at [`SERIAL_MAX_CHARS`]. `None`
/// when nothing is left.
///
/// Applied to each row where the serial is resolved **and again at ingest**, because core cannot
/// assume the poller that sent it is this one. A list already joined with `, ` passes through it
/// unchanged.
#[must_use]
pub fn sanitize(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut count = 0usize;
    let mut gap = false;
    for c in raw.chars() {
        if c.is_whitespace() || c.is_control() {
            gap = count > 0;
            continue;
        }
        let needed = if gap { 2 } else { 1 };
        if count + needed > SERIAL_MAX_CHARS {
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
    use super::*;

    /// `(entPhysicalIndex, entPhysicalClass, entPhysicalSerialNum)` rows, split into the two maps
    /// the poller builds from its walk.
    fn rows(entries: &[(u32, i64, &str)]) -> (BTreeMap<u32, i64>, BTreeMap<u32, String>) {
        let classes = entries.iter().map(|(i, c, _)| (*i, *c)).collect();
        let serials = entries
            .iter()
            .map(|(i, _, s)| (*i, (*s).to_owned()))
            .collect();
        (classes, serials)
    }

    fn resolved(entries: &[(u32, i64, &str)]) -> Option<String> {
        let (classes, serials) = rows(entries);
        resolve(&classes, &serials)
    }

    /// The shape of LibreNMS's `ios_2960x` recording: the stack row at index 1 has no serial, the
    /// members are the chassis rows at 1001, 2001 and 3001, and a component between them carries a
    /// serial of its own that is not the device's.
    #[test]
    fn a_catalyst_stack_lists_every_member_in_index_order() {
        assert_eq!(
            resolved(&[
                (1, 11, ""),
                (1001, 3, "FCW1929B68S"),
                (1002, 9, ""),
                (1006, 9, "DCB192561NL"),
                (3001, 3, "FCW1929B6BP"),
                (2001, 3, "FCW1931A06Z"),
            ])
            .as_deref(),
            Some("FCW1929B68S, FCW1931A06Z, FCW1929B6BP")
        );
    }

    /// The shape of `iosxr_asr9010`: a line card at a lower index than the chassis carries a serial.
    /// The first non-empty row would have taken the card's.
    #[test]
    fn a_line_card_below_the_chassis_index_is_not_taken() {
        assert_eq!(
            resolved(&[
                (4_860_170, 9, "FNS18190KYK"),
                (24_555_730, 3, "FOX1820GVER")
            ])
            .as_deref(),
            Some("FOX1820GVER")
        );
    }

    /// The shape of `routeros_rb433gl`: the chassis row is empty and the only text in the column is
    /// a USB device's name, which is not a serial of this box.
    #[test]
    fn an_empty_chassis_serial_gives_nothing_even_when_another_row_has_text() {
        assert_eq!(
            resolved(&[(65_536, 3, ""), (262_145, 2, "rb400_usb")]),
            None
        );
    }

    /// The shape of `nxos_n9k-c93180yc-fx3`: the chassis serial repeated on the stack and module
    /// rows — the chassis row alone is what is shown.
    #[test]
    fn nx_os_repeating_its_serial_on_other_rows_shows_it_once() {
        assert_eq!(
            resolved(&[
                (10, 11, "FDO2750P"),
                (22, 9, "FDO2750P"),
                (149, 3, "FDO2750P"),
            ])
            .as_deref(),
            Some("FDO2750P")
        );
    }

    #[test]
    fn two_chassis_rows_with_the_same_serial_list_it_once() {
        assert_eq!(
            resolved(&[(1, 3, "ABC123"), (2, 3, "ABC123")]).as_deref(),
            Some("ABC123")
        );
    }

    #[test]
    fn no_chassis_row_gives_nothing() {
        assert_eq!(resolved(&[(1, 9, "MOD1"), (2, 10, "PORT1")]), None);
        assert_eq!(resolved(&[]), None);
    }

    /// A serial row with no class row at the same index is not assumed to be a chassis.
    #[test]
    fn a_serial_with_no_class_beside_it_is_not_taken() {
        let classes = BTreeMap::from([(1, 9)]);
        let serials = BTreeMap::from([(1, "MOD1".to_owned()), (2, "LONELY".to_owned())]);
        assert_eq!(resolve(&classes, &serials), None);
    }

    #[test]
    fn at_most_eight_chassis_are_listed() {
        let entries: Vec<(u32, i64, String)> =
            (1..=10).map(|i| (i, 3, format!("SN{i:02}"))).collect();
        let borrowed: Vec<(u32, i64, &str)> = entries
            .iter()
            .map(|(i, c, s)| (*i, *c, s.as_str()))
            .collect();
        let list = resolved(&borrowed).expect("a list");
        assert_eq!(list, "SN01, SN02, SN03, SN04, SN05, SN06, SN07, SN08");
    }

    /// Three 40-character serials and their separators take 124 characters; a fourth would take
    /// 166. The list stops between serials, so no member is shown cut in half.
    #[test]
    fn the_list_stops_before_a_serial_that_would_not_fit() {
        let long: Vec<String> = (0..5).map(|i| format!("{i}").repeat(40)).collect();
        let entries: Vec<(u32, i64, &str)> = long
            .iter()
            .enumerate()
            .map(|(i, s)| (i as u32 + 1, 3, s.as_str()))
            .collect();
        let list = resolved(&entries).expect("a list");
        assert_eq!(list.split(SEPARATOR).count(), 3, "{list}");
        assert_eq!(list.chars().count(), 124);
        assert!(list.chars().count() <= SERIAL_MAX_CHARS);
    }

    #[test]
    fn a_serial_is_cleaned_before_it_is_listed() {
        assert_eq!(
            resolved(&[(1, 3, "  FCW1929B68S\r\n"), (2, 3, "\t")]).as_deref(),
            Some("FCW1929B68S")
        );
    }

    #[test]
    fn sanitize_folds_whitespace_and_drops_control_characters() {
        assert_eq!(
            sanitize("  SN\u{0}1  2\t3 ").as_deref(),
            Some("SN 1 2 3"),
            "a control character is a gap, runs of whitespace are one space"
        );
        assert_eq!(sanitize(""), None);
        assert_eq!(sanitize(" \r\n\t "), None);
    }

    #[test]
    fn sanitize_cuts_at_the_cap_and_leaves_a_joined_list_alone() {
        let cut = sanitize(&"x".repeat(300)).expect("a value");
        assert_eq!(cut.chars().count(), SERIAL_MAX_CHARS);
        assert_eq!(
            sanitize("FCW1929B68S, FCW1931A06Z").as_deref(),
            Some("FCW1929B68S, FCW1931A06Z")
        );
    }
}
