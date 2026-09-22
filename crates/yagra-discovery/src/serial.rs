// SPDX-License-Identifier: AGPL-3.0-only
//! A device's serial number, read out of a vendor's own MIB or ENTITY-MIB's chassis rows (ADR-147).
//!
//! Pure, like [`crate::os_version`]: the poller holds the SNMP session and walks the columns named
//! here, and this module decides what the rows mean. Five questions, one function each:
//!
//!  - [`vendor_read`] — whether this device's vendor keeps its serial in a MIB of its own, and which
//!    columns to walk for it;
//!  - [`resolve_vendor`] — given those rows, the serial number to show, or `None`;
//!  - [`resolve`] — given ENTITY-MIB's rows, the serial number to show, or `None`;
//!  - [`resolve_huawei_boards`] — given a Huawei's ENTITY-MIB rows with their names and the row each
//!    sits in, its members' main-board serials, or `None` when the chassis rows should decide;
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
//!
//! ## A Huawei lists its members' main boards (Increment 4)
//!
//! A Huawei stack keeps **one** chassis row for the whole stack, and it holds the first member's
//! serial or nothing: measured on a two-member S6730-H (S90003CO01, whose chassis row repeats
//! `MPU Board 0` and whose `MPU Board 1` serial had never been shown), and in LibreNMS's
//! `vrp_5720-vrf`, a three-member S5720 whose chassis row is empty. Each member's serial is on its
//! main board, a module row named `MPU Board N` — `SRU Board N` on a wireless controller or a small
//! router. So a Huawei is walked for `entPhysicalName` as well (decision 17), and those boards are
//! listed by member number (decision 18) — unless a chassis row carries a serial no board does,
//! which is a chassis with a serial of its own, and the chassis rule stands (decision 19).
//!
//! A CloudEngine-branded stack names its boards differently (Increment 5). Walked on the PoC's
//! S90002ds011, a two-member S5735-L-V2 on YunShan OS: each member's main board is called after its
//! model and member number — `S5735-L8P2T4X-A-V2 1` — and sits in a container row named
//! `MPU slot 1`, which sits in the one chassis row. LibreNMS's `vrp_ce12804-entity` uses the same
//! nesting on a modular chassis, with the slot spelled `MPU slot 1/5`. So a name alone cannot say
//! which board is a member's — a model number can end in a digit, and so can any card — but where
//! it sits can: a Huawei is walked for `entPhysicalContainedIn` too (decision 22), and a module
//! directly in a whole-numbered `MPU slot N` of a chassis, whose own name ends in that `N`, is
//! member `N`'s main board (decision 23).

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
/// `entPhysicalName` — the name of an ENTITY-MIB row. Walked beside the class and serial columns on
/// a Huawei only, where it is what tells a member's main board from every other module (Increment 4).
pub const OID_ENT_PHYSICAL_NAME: &str = yagra_common::row_names::ENT_PHYSICAL_NAME;
/// `entPhysicalClass`'s `module(9)` — the class of a board.
pub const CLASS_MODULE: i64 = 9;
/// `entPhysicalContainedIn` — the index of the row this row sits in, `0` for none. Walked beside the
/// name on a Huawei only, where it is what tells a CloudEngine member's main board apart
/// (Increment 5, decision 22).
pub const OID_ENT_PHYSICAL_CONTAINED_IN: &str = "1.3.6.1.2.1.47.1.1.1.1.4";
/// `entPhysicalClass`'s `container(5)` — the class of a slot.
pub const CLASS_CONTAINER: i64 = 5;
/// What a CloudEngine-branded Huawei calls the slot one member's main board sits in, followed by the
/// member number — decision 23. A modular chassis's `MPU slot 1/5` is not one.
const HUAWEI_MEMBER_SLOT: &str = "MPU slot ";
/// Huawei's enterprise number, which a device's `sysObjectID` starts with (Increment 4, decision 17).
const HUAWEI_ENTERPRISE: &str = "1.3.6.1.4.1.2011";
/// What a Huawei calls one member's main board, each followed by the member number: `MPU Board` on a
/// switch stack (S5720, S6730) and `SRU Board` on a wireless controller or small router (AC6605,
/// AR169) — decision 18.
const HUAWEI_BOARD_NAMES: &[&str] = &["MPU Board ", "SRU Board "];

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
    VENDOR_READS
        .iter()
        .find(|read| names_enterprise(sys_object_id, read.enterprise))
}

/// Whether this device's ENTITY-MIB walk also reads `entPhysicalName` and `entPhysicalContainedIn`,
/// for [`resolve_huawei_boards`]: a Huawei, by the enterprise its `sysObjectID` names (Increment 4,
/// decision 17; Increment 5, decision 22). Every other device is walked for the class and serial
/// columns alone.
#[must_use]
pub fn reads_board_names(sys_object_id: Option<&str>) -> bool {
    names_enterprise(sys_object_id, HUAWEI_ENTERPRISE)
}

/// Whether `sys_object_id` sits under `enterprise`. The match must end on an arc boundary —
/// `1.3.6.1.4.1.26360` is not Juniper — and net-snmp's leading-dot spelling is accepted.
fn names_enterprise(sys_object_id: Option<&str>, enterprise: &str) -> bool {
    let Some(oid) = sys_object_id else {
        return false;
    };
    let oid = oid.trim();
    let oid = oid.strip_prefix('.').unwrap_or(oid);
    oid.strip_prefix(enterprise)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
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

/// A Huawei's serial number, read from its members' main boards rather than its chassis row
/// (Increment 4): every module row that is one member's main board, listed by member number and
/// joined the way [`resolve`] joins chassis rows. `None` — so the caller reads the chassis rows —
/// when no such board carries a serial, or when a chassis row carries one that no board does.
///
/// A main board is recognised in either of two shapes: named exactly `MPU Board N` or
/// `SRU Board N` (decision 18), or sitting directly in a chassis's `MPU slot N` and named for that
/// `N`, the way a CloudEngine names it (Increment 5, decision 23). `parents` is
/// `entPhysicalContainedIn` by row; without it only the first shape can be seen.
///
/// The chassis condition is decision 19. A stack's chassis row holds the first member's serial or
/// nothing, so on a stack every chassis serial is also a board's; a chassis row with a serial no
/// board carries is a chassis with a serial of its own, and a slot's board is not what that device
/// is. Without names — every device that is not a Huawei — this is always `None`.
#[must_use]
pub fn resolve_huawei_boards(
    classes: &BTreeMap<u32, i64>,
    names: &BTreeMap<u32, String>,
    parents: &BTreeMap<u32, u32>,
    serials: &BTreeMap<u32, String>,
) -> Option<String> {
    let mut boards: Vec<(u32, u32, String)> = names
        .iter()
        .filter(|(index, _)| classes.get(index) == Some(&CLASS_MODULE))
        .filter_map(|(index, name)| {
            let member = board_member(name)
                .or_else(|| slot_member(*index, name, classes, names, parents))?;
            let serial = serials.get(index).map(String::as_str).and_then(sanitize)?;
            Some((member, *index, serial))
        })
        .collect();
    if boards.is_empty() {
        return None;
    }
    boards.sort_unstable();
    let every_chassis_serial_is_a_board = classes
        .iter()
        .filter(|(_, class)| **class == CLASS_CHASSIS)
        .filter_map(|(index, _)| serials.get(index).map(String::as_str).and_then(sanitize))
        .all(|chassis| boards.iter().any(|(_, _, serial)| *serial == chassis));
    if !every_chassis_serial_is_a_board {
        return None;
    }
    join(boards.iter().map(|(_, _, serial)| serial.as_str()))
}

/// The member number in a Huawei main board's name — `MPU Board 1` is member 1 — or `None` for any
/// other name. The whole name must match (decision 18): `MPU Board 1x`, a padded name and a data
/// centre switch's `CE-MPUA 1/5` are not a member's main board.
fn board_member(name: &str) -> Option<u32> {
    HUAWEI_BOARD_NAMES
        .iter()
        .find_map(|prefix| member_number(name.strip_prefix(prefix)?))
}

/// The member number of a CloudEngine main board — the module at `index`, named `name` — or `None`
/// (Increment 5, decision 23). All four must hold: the row it sits in is a container named exactly
/// `MPU slot N`; that container sits directly in a chassis row; and the board's own name ends in a
/// space and the same `N`, after something. A modular chassis's `MPU slot 1/5`, a sub-card inside a
/// board, and a board filed in a slot numbered differently from its name are none of them.
fn slot_member(
    index: u32,
    name: &str,
    classes: &BTreeMap<u32, i64>,
    names: &BTreeMap<u32, String>,
    parents: &BTreeMap<u32, u32>,
) -> Option<u32> {
    let slot = *parents.get(&index)?;
    if classes.get(&slot) != Some(&CLASS_CONTAINER) {
        return None;
    }
    let digits = names.get(&slot)?.strip_prefix(HUAWEI_MEMBER_SLOT)?;
    let member = member_number(digits)?;
    let chassis = parents.get(&slot)?;
    if classes.get(chassis) != Some(&CLASS_CHASSIS) {
        return None;
    }
    let (model, suffix) = name.rsplit_once(' ')?;
    (!model.trim().is_empty() && suffix == digits).then_some(member)
}

/// A member number written as digits and nothing else, or `None`.
fn member_number(digits: &str) -> Option<u32> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
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

    /// The four maps the poller builds from a Huawei's walk.
    #[derive(Default)]
    struct HuaweiMaps {
        classes: BTreeMap<u32, i64>,
        names: BTreeMap<u32, String>,
        parents: BTreeMap<u32, u32>,
        serials: BTreeMap<u32, String>,
    }

    impl HuaweiMaps {
        /// `(entPhysicalIndex, entPhysicalClass, entPhysicalName, entPhysicalSerialNum)` rows, with
        /// `(entPhysicalIndex, entPhysicalContainedIn)` pairs beside them — empty where the walk that
        /// recorded the rows did not read where each sits.
        fn new(entries: &[(u32, i64, &str, &str)], parents: &[(u32, u32)]) -> Self {
            let mut maps = Self::default();
            for (index, class, name, serial) in entries {
                maps.classes.insert(*index, *class);
                maps.names.insert(*index, (*name).to_owned());
                maps.serials.insert(*index, (*serial).to_owned());
            }
            maps.parents = parents.iter().copied().collect();
            maps
        }

        /// Rows whose containment was not recorded.
        fn flat(entries: &[(u32, i64, &str, &str)]) -> Self {
            Self::new(entries, &[])
        }

        fn boards(&self) -> Option<String> {
            resolve_huawei_boards(&self.classes, &self.names, &self.parents, &self.serials)
        }

        fn chassis_rule(&self) -> Option<String> {
            resolve(&self.classes, &self.serials)
        }
    }

    /// One Huawei device's ENTITY-MIB rows, trimmed to its chassis rows, its main boards, the slots
    /// they sit in and a component or two carrying a serial of their own — with what the chassis rule
    /// alone shows and what is shown now.
    struct HuaweiRecording {
        name: &'static str,
        rows: &'static [(u32, i64, &'static str, &'static str)],
        /// `(entPhysicalIndex, entPhysicalContainedIn)`, for the recordings whose walk read it.
        parents: &'static [(u32, u32)],
        chassis_rule: Option<&'static str>,
        shown: Option<&'static str>,
        /// Whether the main boards decided `shown`, rather than the chassis rows.
        by_boards: bool,
    }

    /// Every shape Increments 4 and 5 were decided on. Two are real devices walked once from the PoC
    /// box — S90003CO01 on 2026-09-15 (no `entPhysicalContainedIn`) and S90002ds011 on 2026-09-16;
    /// the rest are LibreNMS's recordings (`tests/snmpsim/<name>.snmprec`).
    const HUAWEI_RECORDINGS: &[HuaweiRecording] = &[
        HuaweiRecording {
            name: "PoC S90003CO01, a two-member S6730-H48X6C stack",
            rows: &[
                (67_108_867, 3, "HUAWEI S6730 Routing Switch", "1021A0000448"),
                (67_108_873, 9, "MPU Board 0", "1021A0000448"),
                (67_190_797, 9, "POWER Card 0/PWR1", "21021317408NM0000774"),
                (67_223_565, 9, "FAN Card 0/FAN1", ""),
                (68_157_449, 9, "MPU Board 1", "1021A0000352"),
                (68_239_373, 9, "POWER Card 1/PWR1", "21021317408NM0000530"),
            ],
            parents: &[],
            chassis_rule: Some("1021A0000448"),
            shown: Some("1021A0000448, 1021A0000352"),
            by_boards: true,
        },
        HuaweiRecording {
            name:
                "PoC S90002ds011, a two-member CloudEngine S5735-L-V2 stack, boards named by model",
            rows: &[
                (16_777_216, 3, "CloudEngine S5735-L-V2", "QU23C6000037"),
                (16_842_752, 5, "MPU slot 1", ""),
                (16_842_753, 9, "S5735-L8P2T4X-A-V2 1", "QU23C6000037"),
                (16_847_360, 7, "FAN 1/1", ""),
                (16_850_178, 10, "10GE1/0/1", "2000000000529"),
                (16_908_288, 5, "MPU slot 2", ""),
                (16_908_289, 9, "S5735-L8P2T4X-A-V2 2", "QU23C6000056"),
                (16_973_824, 5, "MPU slot 3", ""),
            ],
            parents: &[
                (16_777_216, 0),
                (16_842_752, 16_777_216),
                (16_842_753, 16_842_752),
                (16_847_360, 16_843_520),
                (16_850_178, 16_842_753),
                (16_908_288, 16_777_216),
                (16_908_289, 16_908_288),
                (16_973_824, 16_777_216),
            ],
            chassis_rule: Some("QU23C6000037"),
            shown: Some("QU23C6000037, QU23C6000056"),
            by_boards: true,
        },
        HuaweiRecording {
            name: "vrp_5720-vrf, a three-member S5720 stack with an empty chassis row",
            rows: &[
                (67_108_867, 3, "HUAWEI S5720 Routing Switch", ""),
                (67_108_873, 9, "MPU Board 0", "2102359576DMHC000235"),
                (67_190_797, 9, "POWER Card 0/PWR1", "2102311BXVHVHC001000"),
                (68_157_449, 9, "MPU Board 1", "21359576DMHC000248"),
                (68_173_837, 9, "ES5D21VST000 Card 1/1", "02DWDMHB001394"),
                (69_206_025, 9, "MPU Board 2", "21359576DMHC000230"),
            ],
            parents: &[],
            chassis_rule: None,
            shown: Some("2102359576DMHC000235, 21359576DMHC000248, 21359576DMHC000230"),
            by_boards: true,
        },
        HuaweiRecording {
            name: "vrp_s5328c-ei, one switch with an empty chassis row",
            rows: &[
                (67_108_867, 3, "Quidway S5300 Routing Switch", ""),
                (67_108_873, 9, "MPU Board 0", "21023516106TD5001077"),
                (67_158_029, 9, "FAN Card 0/3", "2102351651N0D5001624"),
            ],
            parents: &[],
            chassis_rule: None,
            shown: Some("21023516106TD5001077"),
            by_boards: true,
        },
        HuaweiRecording {
            name: "vrp_ac6605-26, a wireless controller with an empty chassis row",
            rows: &[
                (3, 3, "AC6605-26-PWR", ""),
                (9, 9, "SRU Board 0", "21023579169WJ6000038"),
            ],
            parents: &[],
            chassis_rule: None,
            shown: Some("21023579169WJ6000038"),
            by_boards: true,
        },
        HuaweiRecording {
            name: "vrp_5720, one switch (a lab simulator), its main board in `Board slot 0`",
            rows: &[
                (
                    67_108_867,
                    3,
                    "HUAWEI S5720 Routing Switch",
                    "2102359576DMHC000120",
                ),
                (67_108_869, 5, "Board slot 0", ""),
                (67_108_873, 9, "MPU Board 0", "2102359576DMHC000120"),
            ],
            parents: &[
                (67_108_867, 0),
                (67_108_869, 67_108_867),
                (67_108_873, 67_108_869),
            ],
            chassis_rule: Some("2102359576DMHC000120"),
            shown: Some("2102359576DMHC000120"),
            by_boards: true,
        },
        HuaweiRecording {
            name: "vrp_ar169sfp, a small router",
            rows: &[
                (3, 3, "AR169F", "21500101573GK1000299"),
                (9, 9, "SRU Board 0", "21500101573GK1000299"),
            ],
            parents: &[],
            chassis_rule: Some("21500101573GK1000299"),
            shown: Some("21500101573GK1000299"),
            by_boards: true,
        },
        HuaweiRecording {
            name: "vrp_ce12804-entity, two chassis whose boards sit in `MPU slot 1/5` and `2/5`",
            rows: &[
                (16_777_216, 3, "CE12804 frame1", "2102113774P0HC000023"),
                (17_104_896, 5, "MPU slot 1/5", ""),
                (17_104_897, 9, "CE-MPUA 1/5", "021NUD6THC600230"),
                (17_760_256, 5, "FAN slot 1/1", ""),
                (17_760_257, 7, "FAN 1/1", "2102120699P0HB003529"),
                (33_554_432, 3, "CE12804 frame2", "2102113774P0HC000040"),
                (33_882_112, 5, "MPU slot 2/5", ""),
                (33_882_113, 9, "CE-MPUA 2/5", "021NUD6THC600151"),
            ],
            parents: &[
                (16_777_216, 0),
                (17_104_896, 16_777_216),
                (17_104_897, 17_104_896),
                (17_760_256, 16_777_216),
                (17_760_257, 17_760_256),
                (33_554_432, 0),
                (33_882_112, 33_554_432),
                (33_882_113, 33_882_112),
            ],
            chassis_rule: Some("2102113774P0HC000023, 2102113774P0HC000040"),
            shown: Some("2102113774P0HC000023, 2102113774P0HC000040"),
            by_boards: false,
        },
    ];

    #[test]
    fn every_huawei_recording_shows_its_members() {
        for recording in HUAWEI_RECORDINGS {
            let maps = HuaweiMaps::new(recording.rows, recording.parents);
            assert_eq!(
                maps.chassis_rule().as_deref(),
                recording.chassis_rule,
                "{} under the chassis rule alone",
                recording.name
            );
            let boards = maps.boards();
            assert_eq!(boards.is_some(), recording.by_boards, "{}", recording.name);
            let shown = boards.or_else(|| maps.chassis_rule());
            assert_eq!(shown.as_deref(), recording.shown, "{}", recording.name);
        }
        assert_eq!(HUAWEI_RECORDINGS.len(), 8);
    }

    /// ADR-147 decision 19: a chassis row carrying a serial no board carries is a chassis with a
    /// serial of its own, so the boards in its slots do not replace it.
    #[test]
    fn a_chassis_with_a_serial_of_its_own_keeps_the_chassis_rule() {
        let maps = HuaweiMaps::flat(&[
            (1, 3, "HUAWEI S12708", "CHASSIS-SN"),
            (2, 9, "MPU Board 7", "MPU-CARD-SN"),
        ]);
        assert_eq!(maps.boards(), None);
        assert_eq!(maps.chassis_rule().as_deref(), Some("CHASSIS-SN"));
    }

    #[test]
    fn only_a_whole_main_board_name_names_a_member() {
        for name in [
            "MPU Board",
            "MPU Board ",
            "MPU Board 1x",
            "MPU Board -1",
            " MPU Board 1",
            "mpu board 1",
            "CE-MPUA 1/5",
            "POWER Card 0/PWR1",
        ] {
            assert_eq!(board_member(name), None, "{name:?}");
        }
        assert_eq!(board_member("MPU Board 0"), Some(0));
        assert_eq!(board_member("SRU Board 12"), Some(12));
    }

    /// Member numbers decide the order, not the row index.
    #[test]
    fn members_are_listed_by_member_number_not_by_row_index() {
        let maps = HuaweiMaps::flat(&[
            (20, 9, "MPU Board 0", "SN-A"),
            (10, 9, "MPU Board 2", "SN-C"),
            (15, 9, "MPU Board 1", "SN-B"),
        ]);
        assert_eq!(maps.boards().as_deref(), Some("SN-A, SN-B, SN-C"));
    }

    /// A row that is named like a main board but is not a module is not one.
    #[test]
    fn a_main_board_name_on_a_row_that_is_not_a_module_is_not_taken() {
        let maps = HuaweiMaps::flat(&[(1, 5, "MPU Board 0", "SN")]);
        assert_eq!(maps.boards(), None);
    }

    /// Main boards that carry no serial leave the chassis rows to decide, exactly as before.
    #[test]
    fn main_boards_with_no_serial_fall_through_to_the_chassis_rows() {
        let maps = HuaweiMaps::flat(&[
            (1, 3, "HUAWEI S5720 Routing Switch", "CH"),
            (2, 9, "MPU Board 0", ""),
            (3, 9, "MPU Board 1", "  "),
        ]);
        assert_eq!(maps.boards(), None);
        assert_eq!(maps.chassis_rule().as_deref(), Some("CH"));
    }

    /// A CloudEngine stack of one member as S90002ds011 nests it — a chassis with no serial of its
    /// own (so decision 19 decides nothing here), a slot in it, and a board in the slot — with the
    /// slot and the board named as given.
    fn cloudengine(slot: &str, board: &str) -> HuaweiMaps {
        HuaweiMaps::new(
            &[
                (1, 3, "CloudEngine S5735-L-V2", ""),
                (2, 5, slot, ""),
                (3, 9, board, "SN-1"),
            ],
            &[(1, 0), (2, 1), (3, 2)],
        )
    }

    /// ADR-147 decision 23: only a slot named exactly `MPU slot` and a whole number is a member's.
    #[test]
    fn a_cloudengine_board_is_a_member_only_in_a_whole_numbered_member_slot() {
        assert_eq!(
            cloudengine("MPU slot 1", "S5735-L8P2T4X-A-V2 1")
                .boards()
                .as_deref(),
            Some("SN-1")
        );
        for slot in [
            "MPU slot 1/5",
            "MPU slot",
            "MPU slot ",
            "MPU slot 1x",
            "MPU slot -1",
            " MPU slot 1",
            "mpu slot 1",
            "LPU slot 1",
            "Board slot 1",
        ] {
            let board = format!(
                "S5735-L8P2T4X-A-V2 {}",
                slot.rsplit(' ').next().unwrap_or("")
            );
            assert_eq!(cloudengine(slot, &board).boards(), None, "{slot:?}");
        }
    }

    /// Decision 23 ⑷: the board carries the number of the slot it sits in, after a name of its own.
    /// A model number that ends in a digit is not the member number.
    #[test]
    fn a_cloudengine_board_must_be_named_for_its_slot() {
        for board in [
            "S5735-L8P2T4X-A-V2 2",
            "S5735-L8P2T4X-A-V2 11",
            "S5735-L8P2T4X-A-V2 01",
            "S5735-L8P2T4X-A-V2-1",
            "S5735-L8P2T4X-A-V2 1 ",
            "S5735-L8P2T4X-A-V21",
            " 1",
            "1",
            "",
        ] {
            assert_eq!(cloudengine("MPU slot 1", board).boards(), None, "{board:?}");
        }
    }

    /// Decision 23 ⑵ and ⑶: the board sits directly in a container, and that container directly in a
    /// chassis — a card inside a main board, or a slot inside a card, is not a member's.
    #[test]
    fn a_member_slot_sits_directly_in_a_chassis() {
        let board = |rows: &[(u32, i64, &str, &str)], parents: &[(u32, u32)]| {
            HuaweiMaps::new(rows, parents).boards()
        };
        // The slot sits in a board, not in the chassis.
        assert_eq!(
            board(
                &[
                    (1, 3, "CloudEngine S5735-L-V2", ""),
                    (2, 9, "S5735-L8P2T4X-A-V2 1", ""),
                    (3, 5, "MPU slot 1", ""),
                    (4, 9, "SUBCARD 1", "SN-SUB"),
                ],
                &[(1, 0), (2, 1), (3, 2), (4, 3)],
            ),
            None
        );
        // The slot sits nowhere, and its parent is not recorded.
        assert_eq!(
            board(
                &[
                    (2, 5, "MPU slot 1", ""),
                    (3, 9, "S5735-L8P2T4X-A-V2 1", "SN-1")
                ],
                &[(3, 2)],
            ),
            None
        );
        assert_eq!(
            board(
                &[
                    (1, 3, "CloudEngine S5735-L-V2", ""),
                    (2, 5, "MPU slot 1", ""),
                    (3, 9, "S5735-L8P2T4X-A-V2 1", "SN-1"),
                ],
                &[(3, 2)],
            ),
            None,
            "the slot's own parent is not recorded"
        );
        // The row the board sits in is named like a slot but is not a container.
        assert_eq!(
            board(
                &[
                    (1, 3, "CloudEngine S5735-L-V2", ""),
                    (2, 9, "MPU slot 1", ""),
                    (3, 9, "S5735-L8P2T4X-A-V2 1", "SN-1"),
                ],
                &[(1, 0), (2, 1), (3, 2)],
            ),
            None
        );
        // A card inside the main board is named for the member too, and is not the main board.
        assert_eq!(
            board(
                &[
                    (1, 3, "CloudEngine S5735-L-V2", ""),
                    (2, 5, "MPU slot 1", ""),
                    (3, 9, "S5735-L8P2T4X-A-V2 1", "SN-1"),
                    (4, 9, "ES5D21VST000 1", "SN-CARD"),
                ],
                &[(1, 0), (2, 1), (3, 2), (4, 3)],
            )
            .as_deref(),
            Some("SN-1")
        );
    }

    /// Without the containment column, a CloudEngine's boards cannot be told apart and the chassis
    /// row decides, as it did before Increment 5.
    #[test]
    fn without_containment_a_cloudengine_stack_keeps_the_chassis_rule() {
        let recording = &HUAWEI_RECORDINGS[1];
        assert!(recording.name.contains("S90002ds011"));
        let maps = HuaweiMaps::flat(recording.rows);
        assert_eq!(maps.boards(), None);
        assert_eq!(maps.chassis_rule().as_deref(), Some("QU23C6000037"));
    }

    /// The shape of the PoC's S90002cs001, a four-member stack of two models — the row indexes and
    /// names are the ones its core stored; the chassis row and the serials are made up, because that
    /// device was not walked. Members are listed by slot, whichever model each is.
    #[test]
    fn a_stack_of_two_models_is_listed_by_member_number() {
        let maps = HuaweiMaps::new(
            &[
                (16_777_216, 3, "CloudEngine S5735-L-V2", "SN-M1"),
                (16_842_752, 5, "MPU slot 1", ""),
                (16_842_753, 9, "S5735-L24T4XE-A-V2 1", "SN-M1"),
                (16_908_288, 5, "MPU slot 2", ""),
                (16_908_289, 9, "S5735-L24T4XE-A-V2 2", "SN-M2"),
                (16_973_824, 5, "MPU slot 3", ""),
                (16_973_825, 9, "S5735-L24ST4XE-A-V2 3", "SN-M3"),
                (17_039_360, 5, "MPU slot 4", ""),
                (17_039_361, 9, "S5735-L24ST4XE-A-V2 4", "SN-M4"),
            ],
            &[
                (16_777_216, 0),
                (16_842_752, 16_777_216),
                (16_842_753, 16_842_752),
                (16_908_288, 16_777_216),
                (16_908_289, 16_908_288),
                (16_973_824, 16_777_216),
                (16_973_825, 16_973_824),
                (17_039_360, 16_777_216),
                (17_039_361, 17_039_360),
            ],
        );
        assert_eq!(maps.boards().as_deref(), Some("SN-M1, SN-M2, SN-M3, SN-M4"));
    }

    #[test]
    fn a_huawei_is_recognised_by_its_enterprise_on_an_arc_boundary() {
        assert!(reads_board_names(Some("1.3.6.1.4.1.2011.2.23.291")));
        assert!(
            reads_board_names(Some(" .1.3.6.1.4.1.2011.2.239.1 ")),
            "net-snmp's leading dot and surrounding whitespace"
        );
        assert!(!reads_board_names(Some("1.3.6.1.4.1.20110.1")));
        assert!(!reads_board_names(Some("1.3.6.1.4.1.2636.1.1.1.2.108")));
        assert!(!reads_board_names(Some("")));
        assert!(!reads_board_names(None));
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
