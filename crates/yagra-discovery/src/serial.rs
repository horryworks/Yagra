// SPDX-License-Identifier: AGPL-3.0-only
//! A device's serial number, read out of a vendor's own MIB or ENTITY-MIB's chassis rows (ADR-147).
//!
//! Pure, like [`crate::os_version`]: the poller holds the SNMP session and walks the columns named
//! here, and this module decides what the rows mean. Four questions, one function each:
//!
//!  - [`vendor_read`] — whether this device's vendor keeps its serial in a MIB of its own, and which
//!    columns to walk for it;
//!  - [`resolve_vendor`] — given those rows, the serial number to show, or `None`;
//!  - [`resolve`] — given ENTITY-MIB's rows, the serial number to show, or `None`;
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
//! One narrow exception (Increment 3, decision 16): a device with **no class column at all** and
//! exactly one distinct non-empty serial shows that serial. Two conditions, not one — "the first
//! non-empty row" is still refused — and a real device is not expected to meet them, since
//! ENTITY-MIB makes the class column mandatory; the lab's trimmed `ios_c3560` recording does.
//!
//! ## A stack shows every member
//!
//! Several chassis rows are joined in index order with `, ` (ADR-147 decision 2). A serial repeated
//! on two rows is listed once, at most [`SERIAL_MAX_CHASSIS`] are listed, and the list stops before
//! a serial that would take it past [`SERIAL_MAX_CHARS`] rather than cutting one in half.
//!
//! ## A vendor's own MIB comes first (Increment 2)
//!
//! Junos keeps no chassis serial in ENTITY-MIB — none of LibreNMS's eight Junos recordings has a
//! single chassis row — but Juniper's own MIB has two: every Virtual Chassis member's, and the box's.
//! A vendor listed in [`vendor_read`]'s table is walked for those first, and ENTITY-MIB is read only
//! when they are both empty (decision 13), so adding a vendor never leaves a device worse off than
//! the chassis rule did. A vendor walk that did not finish sends nothing and does not fall back
//! (decision 12): the box serial alone would replace a Virtual Chassis's whole list.
//!
//! The vendor is recognised by its enterprise number in `sysObjectID`, not by the OS-version table's
//! rows: those also match on `sysDescr` text, which says nothing about whether the vendor's MIB is
//! there (decision 14).

use std::collections::BTreeMap;

/// `entPhysicalClass` — what kind of component an ENTITY-MIB row is.
pub const OID_ENT_PHYSICAL_CLASS: &str = "1.3.6.1.2.1.47.1.1.1.1.5";
/// `entPhysicalSerialNum` — the vendor's serial number for that component.
pub const OID_ENT_PHYSICAL_SERIAL_NUM: &str = "1.3.6.1.2.1.47.1.1.1.1.11";
/// `entPhysicalClass`'s `chassis(3)`.
pub const CLASS_CHASSIS: i64 = 3;
/// `jnxVirtualChassisMemberSerialnumber` — one Juniper Virtual Chassis member's serial, indexed by
/// its member id (JUNIPER-VIRTUALCHASSIS-MIB).
pub const OID_JNX_VC_MEMBER_SERIAL: &str = "1.3.6.1.4.1.2636.3.40.1.4.1.1.1.2";
/// `jnxBoxSerialNo` — the Juniper box's own serial, at instance `.0` (JUNIPER-MIB).
pub const OID_JNX_BOX_SERIAL: &str = "1.3.6.1.4.1.2636.3.1.3";
/// The longest serial — or list of a stack's serials — a node keeps. Longer is cut, never refused.
pub const SERIAL_MAX_CHARS: usize = 128;
/// The most chassis, or Virtual Chassis members, whose serials are listed for one node.
pub const SERIAL_MAX_CHASSIS: usize = 8;

/// What goes between two members' serials.
const SEPARATOR: &str = ", ";

/// How one column of a vendor's own MIB carries a serial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VendorColumn {
    /// Every row is one member's serial, listed in index order — a Virtual Chassis, whose member ids
    /// stay put when mastership moves, so the list does not change with it.
    Members(&'static str),
    /// Only the `.0` instance is the device's serial.
    Scalar(&'static str),
}

impl VendorColumn {
    /// The column this reads, as the poller walks it.
    #[must_use]
    pub fn oid(self) -> &'static str {
        match self {
            Self::Members(oid) | Self::Scalar(oid) => oid,
        }
    }
}

/// A vendor that keeps its serial in a MIB of its own (ADR-147 Increment 2).
#[derive(Debug)]
pub struct VendorRead {
    /// Who this is — the `source` label the poller counts these reads under.
    pub name: &'static str,
    /// The enterprise OID a device's `sysObjectID` starts with, without a leading dot.
    enterprise: &'static str,
    /// Where the rule came from. Read by the tests only, so a row cannot ship without one.
    #[cfg_attr(not(test), allow(dead_code))]
    origin: &'static str,
    /// Walked together in one call, and tried in this order: the first to give a serial wins.
    pub columns: &'static [VendorColumn],
}

/// The vendors [`vendor_read`] knows. A new vendor is one row here.
const VENDOR_READS: &[VendorRead] = &[VendorRead {
    name: "juniper",
    enterprise: "1.3.6.1.4.1.2636",
    origin: "LibreNMS/OS/Junos.php (jnxBoxSerialNo.0), with every JUNIPER-VIRTUALCHASSIS-MIB member listed first (ADR-147 decisions 2 and 11)",
    columns: &[
        VendorColumn::Members(OID_JNX_VC_MEMBER_SERIAL),
        VendorColumn::Scalar(OID_JNX_BOX_SERIAL),
    ],
}];

/// The vendor whose own MIB this device's serial should be read from first, by the enterprise its
/// `sysObjectID` names. `None` for every other device, which is read through ENTITY-MIB alone.
///
/// The enterprise must end on an arc boundary — `1.3.6.1.4.1.26360` is not Juniper — and net-snmp's
/// leading-dot spelling is accepted.
#[must_use]
pub fn vendor_read(sys_object_id: Option<&str>) -> Option<&'static VendorRead> {
    let oid = sys_object_id?.trim();
    let oid = oid.strip_prefix('.').unwrap_or(oid);
    VENDOR_READS.iter().find(|read| {
        oid.strip_prefix(read.enterprise)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
    })
}

/// The serial number a vendor's own MIB gives, from the string rows its walk returned: keyed by
/// column, then by instance index. `None` when none of its columns carries one — the caller then
/// reads ENTITY-MIB (decision 13).
///
/// A column is taken only if it yields a serial after [`sanitize`], so a Virtual Chassis table whose
/// members all report an empty serial falls through to the box serial rather than hiding it.
#[must_use]
pub fn resolve_vendor(
    read: &VendorRead,
    rows: &BTreeMap<String, BTreeMap<u32, String>>,
) -> Option<String> {
    read.columns.iter().find_map(|column| {
        let values = rows.get(column.oid())?;
        match column {
            VendorColumn::Members(_) => join(values.values().map(String::as_str)),
            VendorColumn::Scalar(_) => values.get(&0).and_then(|raw| sanitize(raw)),
        }
    })
}

/// The serial number to show for a device, from its ENTITY-MIB rows keyed by `entPhysicalIndex`:
/// every chassis row's serial, in index order, joined with `, `. `None` when no chassis row
/// carries one.
///
/// A serial with no class row beside it is not taken, and a chassis row with an empty serial is
/// skipped rather than listed as a blank — a RouterOS box reports exactly that, with its only
/// non-empty serial on a USB device.
///
/// One exception, for a device with **no class column at all** (Increment 3, decision 16): when
/// `classes` is empty and the serial column holds exactly one distinct non-empty value, that value
/// is the device — there is nothing to tell the rows apart, one candidate is unambiguous, and two
/// would be a guess. The lab's `ios_c3560` recording is the case: LibreNMS trimmed the class column
/// out and left one serial, `CAT0912N0CU`. A real device implements the class column (ENTITY-MIB
/// makes it mandatory), so class rows present and no chassis among them stays `None`.
#[must_use]
pub fn resolve(classes: &BTreeMap<u32, i64>, serials: &BTreeMap<u32, String>) -> Option<String> {
    if classes.is_empty() {
        let mut distinct: Vec<String> = Vec::new();
        for serial in serials.values().filter_map(|s| sanitize(s)) {
            if !distinct.contains(&serial) {
                distinct.push(serial);
            }
            if distinct.len() > 1 {
                return None;
            }
        }
        return distinct.pop();
    }
    join(
        classes
            .iter()
            .filter(|(_, class)| **class == CLASS_CHASSIS)
            .filter_map(|(index, _)| serials.get(index).map(String::as_str)),
    )
}

/// Several members' serials as one value, in the order given: each cleaned by [`sanitize`], empty
/// ones skipped, a repeat listed once, at most [`SERIAL_MAX_CHASSIS`], and stopping before a serial
/// that would take the list past [`SERIAL_MAX_CHARS`]. `None` when nothing is left.
fn join<'a>(raw: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut listed: Vec<String> = Vec::new();
    let mut chars = 0usize;
    for serial in raw.into_iter().filter_map(sanitize) {
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
        // A class column that is present and names no chassis is not one that is absent: the one
        // serial here is a module's, and its class row says so (decision 16 does not apply).
        assert_eq!(resolved(&[(1, 9, "MOD1")]), None);
    }

    /// A serial row with no class row at the same index is not assumed to be a chassis.
    #[test]
    fn a_serial_with_no_class_beside_it_is_not_taken() {
        let classes = BTreeMap::from([(1, 9)]);
        let serials = BTreeMap::from([(1, "MOD1".to_owned()), (2, "LONELY".to_owned())]);
        assert_eq!(resolve(&classes, &serials), None);
    }

    /// The shape of `ios_c3560` as LibreNMS recorded it: no class column at all, and one serial
    /// (Increment 3, decision 16). With nothing to tell the rows apart, one candidate is the device.
    #[test]
    fn a_lone_serial_with_no_class_column_is_taken() {
        let none = BTreeMap::new();
        let serials = BTreeMap::from([(1001, "CAT0912N0CU".to_owned())]);
        assert_eq!(resolve(&none, &serials).as_deref(), Some("CAT0912N0CU"));
        // A blank row is not a candidate, so it does not make the one serial ambiguous.
        let serials = BTreeMap::from([(1, "  ".to_owned()), (1001, "CAT0912N0CU".to_owned())]);
        assert_eq!(resolve(&none, &serials).as_deref(), Some("CAT0912N0CU"));
    }

    /// One serial *repeated* is still one candidate — NX-OS writes its chassis serial on the stack
    /// and module rows too — while two different serials are a guess, and a guess is not taken.
    #[test]
    fn with_no_class_column_only_one_distinct_serial_is_unambiguous() {
        let none = BTreeMap::new();
        let repeated = BTreeMap::from([
            (10, "FDO2750P".to_owned()),
            (22, "FDO2750P".to_owned()),
            (149, "FDO2750P".to_owned()),
        ]);
        assert_eq!(resolve(&none, &repeated).as_deref(), Some("FDO2750P"));
        let two = BTreeMap::from([(1, "CHASSIS1".to_owned()), (2, "MODULE1".to_owned())]);
        assert_eq!(resolve(&none, &two), None);
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

    fn juniper() -> &'static VendorRead {
        vendor_read(Some("1.3.6.1.4.1.2636.1.1.1.2.108")).expect("Juniper is listed")
    }

    /// A Juniper walk's rows: the Virtual Chassis members as `(member id, serial)` and the box row
    /// at `.0`. A column the device does not implement walks to no rows, so it has no entry.
    fn juniper_rows(
        members: &[(u32, &str)],
        box_serial: Option<&str>,
    ) -> BTreeMap<String, BTreeMap<u32, String>> {
        let mut rows = BTreeMap::new();
        if !members.is_empty() {
            rows.insert(
                OID_JNX_VC_MEMBER_SERIAL.to_owned(),
                members
                    .iter()
                    .map(|(id, s)| (*id, (*s).to_owned()))
                    .collect(),
            );
        }
        if let Some(serial) = box_serial {
            rows.insert(
                OID_JNX_BOX_SERIAL.to_owned(),
                BTreeMap::from([(0, serial.to_owned())]),
            );
        }
        rows
    }

    /// One of LibreNMS's Junos recordings (`tests/snmpsim/<name>.snmprec` at `dc125ecd`), trimmed to
    /// the two Juniper columns, with the serial LibreNMS derived from it (`tests/data/<name>.json`).
    struct JunosRecording {
        name: &'static str,
        sys_object_id: &'static str,
        members: &'static [(u32, &'static str)],
        box_serial: Option<&'static str>,
        librenms: Option<&'static str>,
    }

    const JUNOS_RECORDINGS: &[JunosRecording] = &[
        JunosRecording {
            name: "junos",
            sys_object_id: "1.3.6.1.4.1.2636.1.1.1.2.41",
            members: &[(0, "BM0215390011"), (1, "BM0209337513")],
            box_serial: None,
            librenms: None,
        },
        JunosRecording {
            name: "junos_ber",
            sys_object_id: "1.3.6.1.4.1.2636.1.1.1.2.108",
            members: &[],
            box_serial: None,
            librenms: None,
        },
        JunosRecording {
            name: "junos_ex",
            sys_object_id: "1.3.6.1.4.1.2636.1.1.1.4.131.2",
            members: &[],
            box_serial: None,
            librenms: None,
        },
        JunosRecording {
            name: "junos_ex3300-12.3r4.6",
            sys_object_id: "1.3.6.1.4.1.2636.1.1.1.2.76",
            members: &[],
            box_serial: Some("GA0222127204"),
            librenms: Some("GA0222127204"),
        },
        JunosRecording {
            name: "junos_ex3400-15.1x53-d55.5",
            sys_object_id: "1.3.6.1.4.1.2636.1.1.1.4.131.2",
            members: &[],
            box_serial: Some("NX0290690152"),
            librenms: Some("NX0290690152"),
        },
        JunosRecording {
            name: "junos_ex4200-vc-12.3r11.2",
            sys_object_id: "1.3.6.1.4.1.2636.1.1.1.2.31",
            members: &[],
            box_serial: Some("BP0258337258"),
            librenms: Some("BP0258337258"),
        },
        JunosRecording {
            name: "junos_ex4600mp",
            sys_object_id: "1.3.6.1.4.1.2636.1.1.1.4.63.9",
            members: &[
                (0, "XR0123456789"),
                (1, "XR0123456790"),
                (2, "XR0123456791"),
                (3, "XR0123456792"),
                (4, "XR0123456793"),
                (5, "XR0123456794"),
                (6, "XR0123456795"),
                (7, "XR0123456796"),
            ],
            box_serial: Some("XR0123456789"),
            librenms: Some("XR0123456789"),
        },
        JunosRecording {
            name: "junos_vmx",
            sys_object_id: "1.3.6.1.4.1.2636.1.1.1.2.108",
            members: &[],
            box_serial: Some("VM600B272BD3"),
            librenms: Some("VM600B272BD3"),
        },
    ];

    /// What each recording must show. Where it differs from LibreNMS the difference is one this
    /// module chose: LibreNMS reads `jnxBoxSerialNo.0` alone, and a Virtual Chassis here lists every
    /// member (ADR-147 decisions 2 and 11) — which also gives `junos`, whose box row is absent, a
    /// serial at all.
    fn expected(recording: &JunosRecording) -> Option<&'static str> {
        match recording.name {
            "junos" => Some("BM0215390011, BM0209337513"),
            "junos_ex4600mp" => Some(
                "XR0123456789, XR0123456790, XR0123456791, XR0123456792, \
                 XR0123456793, XR0123456794, XR0123456795, XR0123456796",
            ),
            _ => recording.librenms,
        }
    }

    #[test]
    fn every_librenms_junos_recording_resolves_to_its_serial() {
        let mut deviations = Vec::new();
        for recording in JUNOS_RECORDINGS {
            let read = vendor_read(Some(recording.sys_object_id))
                .unwrap_or_else(|| panic!("{} is not read as Juniper", recording.name));
            let got = resolve_vendor(read, &juniper_rows(recording.members, recording.box_serial));
            assert_eq!(got.as_deref(), expected(recording), "{}", recording.name);
            if got.as_deref() != recording.librenms {
                deviations.push(recording.name);
            }
        }
        assert_eq!(JUNOS_RECORDINGS.len(), 8);
        assert_eq!(
            deviations,
            ["junos", "junos_ex4600mp"],
            "only the two Virtual Chassis recordings may answer differently from LibreNMS"
        );
    }

    #[test]
    fn a_vendor_is_recognised_by_its_enterprise_on_an_arc_boundary() {
        assert_eq!(
            vendor_read(Some("1.3.6.1.4.1.2636.1.1.1.2.108")).map(|r| r.name),
            Some("juniper")
        );
        assert_eq!(
            vendor_read(Some(" .1.3.6.1.4.1.2636.1.1.1.4.63.9 ")).map(|r| r.name),
            Some("juniper"),
            "net-snmp's leading dot and surrounding whitespace"
        );
        assert!(vendor_read(Some("1.3.6.1.4.1.26360.1")).is_none());
        assert!(vendor_read(Some("1.3.6.1.4.1.9.1.1208")).is_none());
        assert!(vendor_read(Some("")).is_none());
        assert!(vendor_read(None).is_none());
    }

    /// Member ids, not the order the rows arrived in and not who is master, decide the order; a
    /// member reporting an empty serial is skipped rather than listed as a blank.
    #[test]
    fn virtual_chassis_members_are_listed_by_member_id_and_empty_ones_skipped() {
        let rows = juniper_rows(&[(2, "SN-C"), (0, "SN-A"), (1, " ")], None);
        assert_eq!(
            resolve_vendor(juniper(), &rows).as_deref(),
            Some("SN-A, SN-C")
        );
    }

    /// A Virtual Chassis table whose members are all empty does not hide the box serial.
    #[test]
    fn empty_members_fall_through_to_the_box_serial() {
        let rows = juniper_rows(&[(0, ""), (1, "")], Some("BOX1"));
        assert_eq!(resolve_vendor(juniper(), &rows).as_deref(), Some("BOX1"));
    }

    #[test]
    fn only_instance_zero_of_the_box_column_is_the_serial() {
        let mut rows = BTreeMap::new();
        rows.insert(
            OID_JNX_BOX_SERIAL.to_owned(),
            BTreeMap::from([(1, "NOT-THE-BOX".to_owned())]),
        );
        assert_eq!(resolve_vendor(juniper(), &rows), None);
        rows.get_mut(OID_JNX_BOX_SERIAL)
            .expect("the column")
            .insert(0, "BOX1".to_owned());
        assert_eq!(resolve_vendor(juniper(), &rows).as_deref(), Some("BOX1"));
    }

    #[test]
    fn no_rows_from_a_vendor_gives_nothing() {
        assert_eq!(resolve_vendor(juniper(), &BTreeMap::new()), None);
    }

    #[test]
    fn every_vendor_read_names_its_origin_and_columns() {
        for read in VENDOR_READS {
            assert!(!read.origin.is_empty(), "{}", read.name);
            assert!(!read.columns.is_empty(), "{}", read.name);
            assert!(
                !read.enterprise.starts_with('.') && !read.enterprise.ends_with('.'),
                "{}",
                read.name
            );
        }
    }
}
