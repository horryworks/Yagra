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

use std::borrow::Cow;
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
/// Sized for [`SERIAL_MAX_CHASSIS`] serials of 22 characters with their separators (Inc.11
/// decision 38); the longest measured is 20.
pub const SERIAL_MAX_CHARS: usize = 384;
/// The most chassis, or Virtual Chassis members, whose serials are listed for one node. Sixteen
/// since Inc.11 decision 38: a Nexus 5010 with ten FEX is eleven chassis rows, and eight cut three
/// of them off.
pub const SERIAL_MAX_CHASSIS: usize = 16;
/// `entPhysicalModelName` — the vendor's model name for a component (Increment 8).
pub const OID_ENT_PHYSICAL_MODEL_NAME: &str = "1.3.6.1.2.1.47.1.1.1.1.13";
/// The longest one member's model is kept at. The longest measured is 20 characters.
pub const MODEL_ITEM_MAX_CHARS: usize = 64;
/// The longest a node's model — a stack's whole list — is kept at: every member at its longest,
/// with the separators between them, so the list never stops before the serial list does.
pub const MODEL_MAX_CHARS: usize =
    SERIAL_MAX_CHASSIS * MODEL_ITEM_MAX_CHARS + (SERIAL_MAX_CHASSIS - 1) * SEPARATOR.len();
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
/// `entPhysicalClass`'s `sensor(8)` — read by the poller's media walk, which must never take a
/// sensor for a transceiver.
pub const CLASS_SENSOR: i64 = 8;
/// `entPhysicalClass`'s `port(10)` — read by the poller's media walk to find port entities.
pub const CLASS_PORT: i64 = 10;
/// What a CloudEngine-branded Huawei calls the slot one member's main board sits in, followed by the
/// member number — decision 23. A modular chassis's `MPU slot 1/5` is not one.
const HUAWEI_MEMBER_SLOT: &str = "MPU slot ";
/// What a CloudEngine-branded Huawei calls its chassis row, followed by the series (Inc.8).
const HUAWEI_CLOUDENGINE: &str = "CloudEngine ";
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

/// `entPhysicalVendorType` — what kind of component an ENTITY-MIB row is, as an OID the vendor
/// registers. Walked on NX-OS only, where a FEX chassis row's model column is a description and
/// this column is what names the part (Inc.11 decision 39).
pub const OID_ENT_PHYSICAL_VENDOR_TYPE: &str = yagra_common::OID_ENT_PHYSICAL_VENDOR_TYPE;

/// `cevChassis` in CISCO-ENTITY-VENDORTYPE-OID-MIB: Cisco's chassis vendor types, and the arc an
/// NX-OS switch's own `sysObjectID` sits under.
const CEV_CHASSIS: &str = "1.3.6.1.4.1.9.12.3.1.3";

/// The Nexus 2000 Fabric Extenders whose part number a FEX chassis row's vendor type names, by
/// their arc under [`CEV_CHASSIS`] (Inc.11 decision 39).
///
/// NX-OS answers a FEX chassis row's `entPhysicalModelName` with a description (`Fabric Extender
/// Module: 48x1GE, 4x10GE`) and puts the part nowhere in ENTITY-MIB, so the vendor type is the only
/// thing that says which FEX it is. `914` was measured on a Nexus 5010 running 5.0(2)N2(1) with ten
/// FEX; the other rows are read from the public MIB's names and comments, taking only those whose
/// part number they spell unambiguously.
const FEX_PARTS: &[(u32, &str)] = &[
    (820, "N2K-C2148T-1GE"),
    (913, "N2K-C2232PP-10GE"),
    (914, "N2K-C2248TP-1GE"),
    (1006, "N2K-C2224TP-1GE"),
    (1046, "N2K-C2248TP-E-1GE"),
    (1058, "N2K-B22HP-P"),
    (1061, "N2K-C2232TM-10GE"),
    (1190, "N2K-C2232TM-E-10GE"),
    (1205, "N2K-C2248PQ-10GE"),
    (1210, "N2K-B22FTS-P"),
    (1211, "N2K-B22DELL-P"),
    (1339, "N2K-B22IBM-P"),
    (1541, "N2K-C2348UPQ"),
];

/// Whether this device's ENTITY-MIB is walked for `entPhysicalVendorType` too: an NX-OS switch,
/// by its `sysObjectID` sitting under [`CEV_CHASSIS`] (Inc.11 decision 39).
#[must_use]
pub fn reads_vendor_types(sys_object_id: Option<&str>) -> bool {
    names_enterprise(sys_object_id, CEV_CHASSIS)
}

/// The FEX part number a vendor type names, or `None` when it is not a FEX in [`FEX_PARTS`].
fn fex_part(vendor_type: &str) -> Option<&'static str> {
    let oid = vendor_type.trim();
    let oid = oid.strip_prefix('.').unwrap_or(oid);
    let arc: u32 = oid
        .strip_prefix(CEV_CHASSIS)?
        .strip_prefix('.')?
        .parse()
        .ok()?;
    FEX_PARTS
        .iter()
        .find(|(listed, _)| *listed == arc)
        .map(|(_, part)| *part)
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
    members(&chassis_rows(classes), serials, None).serial
}

/// The `entPhysicalIndex` of every chassis row, in index order — the rows [`resolve`] reads.
fn chassis_rows(classes: &BTreeMap<u32, i64>) -> Vec<u32> {
    classes
        .iter()
        .filter(|(_, class)| **class == CLASS_CHASSIS)
        .map(|(index, _)| *index)
        .collect()
}

/// What ENTITY-MIB says the device is (Increment 8): its serial number and its hardware model,
/// both read off the same rows, so the i-th model belongs to the member whose serial is i-th
/// (decision 30).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntityIdentity {
    /// What [`resolve`] or [`resolve_huawei_boards`] would answer.
    pub serial: Option<String>,
    /// Every member's `entPhysicalModelName`, in the serial's order, or `None` when any member
    /// listed has none — a list that skipped one would pair the rest with the wrong serials.
    pub model: Option<String>,
    /// The rows were a Huawei's main boards (Increments 4 and 5) rather than chassis rows.
    pub from_boards: bool,
}

/// ENTITY-MIB's columns as the poller walks them, each keyed by `entPhysicalIndex`. `names` and
/// `parents` are walked on a Huawei only, and `models` on every device (Increment 8).
#[derive(Debug, Clone, Copy)]
pub struct EntityRows<'a> {
    pub classes: &'a BTreeMap<u32, i64>,
    pub serials: &'a BTreeMap<u32, String>,
    pub names: &'a BTreeMap<u32, String>,
    pub parents: &'a BTreeMap<u32, u32>,
    pub models: &'a BTreeMap<u32, String>,
    /// `entPhysicalVendorType`, dotted, walked on NX-OS only (Inc.11) — empty everywhere else.
    pub vendor_types: &'a BTreeMap<u32, String>,
}

/// The serial and the model ENTITY-MIB gives (Increment 8): a Huawei's main boards when
/// [`resolve_huawei_boards`] would take them, otherwise the chassis rows [`resolve`] reads.
///
/// The serial is exactly what those two functions answer. The model is listed only from the rows
/// whose serial was listed, in the same order, so a repeat or an empty serial drops the member from
/// both lists at once (decision 30). A device whose chassis rows carry a model but no serial still
/// shows the model. A device with no class column shows none: nothing says which row is the device.
#[must_use]
pub fn resolve_entity(rows: EntityRows<'_>) -> EntityIdentity {
    if let Some(boards) = huawei_board_rows(rows.classes, rows.names, rows.parents, rows.serials) {
        // A board's model is the device's only on a CloudEngine stack, where each member is a box
        // of its own (decision 34). A USG files its MPU card in the same `MPU slot 0` shape, and
        // that card's model (`MPUA-USG6530F-D`) is a part number, not what the firewall is.
        let models = names_a_cloudengine(rows.classes, rows.names)
            .then(|| cloudengine_board_models(&boards, rows));
        return EntityIdentity {
            from_boards: true,
            ..members(&boards, rows.serials, models.as_ref())
        };
    }
    if rows.classes.is_empty() {
        return EntityIdentity {
            serial: resolve(rows.classes, rows.serials),
            ..EntityIdentity::default()
        };
    }
    let models = chassis_models(rows);
    members(&chassis_rows(rows.classes), rows.serials, Some(&models))
}

/// The model column as the chassis rows should read it.
///
/// - A chassis row whose vendor type names a Nexus FEX takes that FEX's part number (Inc.11
///   decision 39): NX-OS answers the model column of a FEX chassis row with a description.
/// - Otherwise a chassis row whose own `entPhysicalModelName` is empty takes the model of the
///   module row carrying the same serial (Inc.10 decision 37). Measured on a Catalyst 4948E
///   (IOS 12.2(54)SG): the chassis row at 1 answers its serial and an empty model, while the
///   supervisor module at 1000 answers the same serial and `WS-C4948E`. A matching serial is what
///   makes the two rows one physical thing, so no other part's model can be borrowed. A chassis
///   with a model of its own keeps it, one with no serial borrows nothing, and two matching
///   modules that disagree give nothing rather than a guess.
fn chassis_models(rows: EntityRows<'_>) -> Cow<'_, BTreeMap<u32, String>> {
    let mut filled: Option<BTreeMap<u32, String>> = None;
    for index in chassis_rows(rows.classes) {
        if let Some(part) = rows.vendor_types.get(&index).and_then(|vt| fex_part(vt)) {
            filled
                .get_or_insert_with(|| rows.models.clone())
                .insert(index, part.to_owned());
            continue;
        }
        let has_own = rows
            .models
            .get(&index)
            .and_then(|raw| sanitize_model_item(raw))
            .is_some();
        if has_own {
            continue;
        }
        let Some(serial) = rows.serials.get(&index).and_then(|raw| sanitize(raw)) else {
            continue;
        };
        if let Some(model) = module_model_with_serial(rows, &serial) {
            filled
                .get_or_insert_with(|| rows.models.clone())
                .insert(index, model);
        }
    }
    filled.map_or(Cow::Borrowed(rows.models), Cow::Owned)
}

/// The one model the module rows carrying `serial` agree on, or `None` when none answers a model
/// or two answer different ones.
fn module_model_with_serial(rows: EntityRows<'_>, serial: &str) -> Option<String> {
    let mut found: Option<String> = None;
    for (index, class) in rows.classes {
        if *class != CLASS_MODULE {
            continue;
        }
        let same_serial = rows
            .serials
            .get(index)
            .and_then(|raw| sanitize(raw))
            .is_some_and(|candidate| candidate == serial);
        if !same_serial {
            continue;
        }
        let Some(model) = rows
            .models
            .get(index)
            .and_then(|raw| sanitize_model_item(raw))
        else {
            continue;
        };
        match &found {
            None => found = Some(model),
            Some(seen) if *seen == model => {}
            Some(_) => return None,
        }
    }
    found
}

/// Each CloudEngine main board's model (Inc.9 decision 36): its `entPhysicalModelName` when it
/// answers one, otherwise its own name with the trailing member number taken off —
/// `S5735-L24T4XE-A-V2 1` is an `S5735-L24T4XE-A-V2`. A YunShan V600R022 answers No Such Object for
/// the whole model column, and the name is the only place it says what each member is. Only a
/// board in the Increment 5 shape gives its name: an `MPU Board N` is named for its slot, not its
/// model.
fn cloudengine_board_models(boards: &[u32], rows: EntityRows<'_>) -> BTreeMap<u32, String> {
    boards
        .iter()
        .filter_map(|&index| {
            if let Some(model) = rows
                .models
                .get(&index)
                .filter(|raw| sanitize_model_item(raw).is_some())
            {
                return Some((index, model.clone()));
            }
            let name = rows.names.get(&index)?;
            slot_member(index, name, rows.classes, rows.names, rows.parents)?;
            let (model, _) = name.rsplit_once(' ')?;
            Some((index, model.to_owned()))
        })
        .collect()
}

/// Whether a chassis row is named for Huawei's CloudEngine brand — `CloudEngine S5735-L-V2`, the
/// one shape measured whose main boards carry the member's own model (Inc.8 decision 34).
fn names_a_cloudengine(classes: &BTreeMap<u32, i64>, names: &BTreeMap<u32, String>) -> bool {
    chassis_rows(classes).iter().any(|index| {
        names
            .get(index)
            .is_some_and(|name| name.trim_start().starts_with(HUAWEI_CLOUDENGINE))
    })
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
    let boards = huawei_board_rows(classes, names, parents, serials)?;
    members(&boards, serials, None).serial
}

/// The rows [`resolve_huawei_boards`] reads, in member order, or `None` when it should not decide —
/// shared with [`resolve_entity`] so the serial and the model are read off the same boards.
fn huawei_board_rows(
    classes: &BTreeMap<u32, i64>,
    names: &BTreeMap<u32, String>,
    parents: &BTreeMap<u32, u32>,
    serials: &BTreeMap<u32, String>,
) -> Option<Vec<u32>> {
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
    Some(boards.into_iter().map(|(_, index, _)| index).collect())
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
    let listed: Vec<String> = kept(raw.into_iter().map(|serial| (serial, ())))
        .into_iter()
        .map(|(serial, ())| serial)
        .collect();
    (!listed.is_empty()).then(|| listed.join(SEPARATOR))
}

/// The members [`join`] lists, each with what was carried beside its serial — the one place the
/// rules for which members are listed live, so a model list built from what this keeps cannot
/// fall out of step with the serial list (decision 30).
fn kept<'a, T>(raw: impl IntoIterator<Item = (&'a str, T)>) -> Vec<(String, T)> {
    let mut listed: Vec<(String, T)> = Vec::new();
    let mut chars = 0usize;
    for (raw, carried) in raw {
        let Some(serial) = sanitize(raw) else {
            continue;
        };
        if listed.iter().any(|(seen, _)| *seen == serial) {
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
        listed.push((serial, carried));
        if listed.len() == SERIAL_MAX_CHASSIS {
            break;
        }
    }
    listed
}

/// The serial and model lists of the members at `rows`, in that order (Increment 8). With no
/// `models`, only the serial is answered — the serial-only readers.
///
/// The model list is built from the members [`kept`] keeps, so it is the serial list's twin, and it
/// is `None` when one of them has no model. When no member has a serial at all, the model list is
/// built from every row instead, under the same all-or-nothing rule — a chassis with a model and no
/// serial is still the model it says it is.
fn members(
    rows: &[u32],
    serials: &BTreeMap<u32, String>,
    models: Option<&BTreeMap<u32, String>>,
) -> EntityIdentity {
    let model_of = |index: u32| {
        models
            .and_then(|m| m.get(&index))
            .and_then(|raw| sanitize_model_item(raw))
    };
    let listed = kept(rows.iter().filter_map(|index| {
        serials
            .get(index)
            .map(|serial| (serial.as_str(), model_of(*index)))
    }));
    let serial = (!listed.is_empty()).then(|| {
        listed
            .iter()
            .map(|(serial, _)| serial.as_str())
            .collect::<Vec<_>>()
            .join(SEPARATOR)
    });
    let model_list: Option<Vec<String>> = if models.is_none() {
        None
    } else if listed.is_empty() {
        rows.iter()
            .take(SERIAL_MAX_CHASSIS)
            .map(|index| model_of(*index))
            .collect()
    } else {
        listed.into_iter().map(|(_, model)| model).collect()
    };
    EntityIdentity {
        serial,
        model: model_list
            .filter(|m| !m.is_empty())
            .map(|m| m.join(SEPARATOR)),
        from_boards: false,
    }
}

/// One member's model, cleaned the way a serial is and cut at [`MODEL_ITEM_MAX_CHARS`].
fn sanitize_model_item(raw: &str) -> Option<String> {
    crate::text::fold_and_cap(raw, MODEL_ITEM_MAX_CHARS)
}

/// Make a device-supplied model — or a stack's list of them — safe to store and show, cut at
/// [`MODEL_MAX_CHARS`] (Increment 8). Applied again at ingest, like [`sanitize`]. The cap fits
/// [`SERIAL_MAX_CHASSIS`] members of [`MODEL_ITEM_MAX_CHARS`] each, so a list is never cut short of
/// the serial list it pairs with.
#[must_use]
pub fn sanitize_model(raw: &str) -> Option<String> {
    crate::text::fold_and_cap(raw, MODEL_MAX_CHARS)
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
    crate::text::fold_and_cap(raw, SERIAL_MAX_CHARS)
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

    /// Sixteen since Inc.11 decision 38 — a Nexus 5010 with ten FEX is eleven chassis rows.
    #[test]
    fn at_most_sixteen_chassis_are_listed() {
        let entries: Vec<(u32, i64, String)> =
            (1..=20).map(|i| (i, 3, format!("SN{i:02}"))).collect();
        let borrowed: Vec<(u32, i64, &str)> = entries
            .iter()
            .map(|(i, c, s)| (*i, *c, s.as_str()))
            .collect();
        let list = resolved(&borrowed).expect("a list");
        let expected: Vec<String> = (1..=16).map(|i| format!("SN{i:02}")).collect();
        assert_eq!(list, expected.join(SEPARATOR));
    }

    /// Nine 40-character serials and their separators take 376 characters; a tenth would take
    /// 418. The list stops between serials, so no member is shown cut in half.
    #[test]
    fn the_list_stops_before_a_serial_that_would_not_fit() {
        let long: Vec<String> = (0..12).map(|i| format!("{}", i % 10).repeat(40)).collect();
        let entries: Vec<(u32, i64, &str)> = long
            .iter()
            .enumerate()
            .map(|(i, s)| (i as u32 + 1, 3, s.as_str()))
            .collect();
        let list = resolved(&entries).expect("a list");
        assert_eq!(list.split(SEPARATOR).count(), 9, "{list}");
        assert_eq!(list.chars().count(), 376);
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
        let cut = sanitize(&"x".repeat(SERIAL_MAX_CHARS + 100)).expect("a value");
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
        /// `entPhysicalModelName`, walked on every device since Increment 8.
        models: BTreeMap<u32, String>,
    }

    impl HuaweiMaps {
        /// The same rows with `entPhysicalModelName` beside them.
        fn with_models(mut self, models: &[(u32, &str)]) -> Self {
            self.models = models.iter().map(|(i, m)| (*i, (*m).to_owned())).collect();
            self
        }

        fn entity(&self) -> EntityIdentity {
            resolve_entity(EntityRows {
                classes: &self.classes,
                serials: &self.serials,
                names: &self.names,
                parents: &self.parents,
                models: &self.models,
                vendor_types: &BTreeMap::new(),
            })
        }

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
        // Increment 8: the boards carry their model, the chassis row does not, and the model list
        // follows the serial list member for member — two of each model, never merged.
        let read = maps
            .with_models(&[
                (16_777_216, ""),
                (16_842_753, "S5735-L24T4XE-A-V2"),
                (16_908_289, "S5735-L24T4XE-A-V2"),
                (16_973_825, "S5735-L24ST4XE-A-V2"),
                (17_039_361, "S5735-L24ST4XE-A-V2"),
            ])
            .entity();
        assert_eq!(read.serial.as_deref(), Some("SN-M1, SN-M2, SN-M3, SN-M4"));
        assert_eq!(
            read.model.as_deref(),
            Some(
                "S5735-L24T4XE-A-V2, S5735-L24T4XE-A-V2, S5735-L24ST4XE-A-V2, S5735-L24ST4XE-A-V2"
            )
        );
        assert!(read.from_boards);
    }

    /// `(entPhysicalIndex, entPhysicalClass, entPhysicalSerialNum, entPhysicalModelName)` rows read
    /// through [`resolve_entity`], for a device that is not a Huawei (no names, no parents).
    fn entity_of(entries: &[(u32, i64, &str, &str)]) -> EntityIdentity {
        let classes = entries.iter().map(|(i, c, _, _)| (*i, *c)).collect();
        let serials = entries
            .iter()
            .map(|(i, _, s, _)| (*i, (*s).to_owned()))
            .collect();
        let models = entries
            .iter()
            .map(|(i, _, _, m)| (*i, (*m).to_owned()))
            .collect();
        resolve_entity(EntityRows {
            classes: &classes,
            serials: &serials,
            names: &BTreeMap::new(),
            parents: &BTreeMap::new(),
            models: &models,
            vendor_types: &BTreeMap::new(),
        })
    }

    /// The shape measured on a C2960X (2026-11-10): one chassis row at 1001 carries the hardware
    /// model, while `sysDescr` only names the software family `C2960X`.
    #[test]
    fn a_cisco_chassis_row_gives_its_hardware_model() {
        let read = entity_of(&[
            (1001, 3, "FOC0000A1AA", "WS-C2960X-24TS-LL"),
            (1002, 9, "", ""),
            (1003, 10, "", ""),
        ]);
        assert_eq!(read.serial.as_deref(), Some("FOC0000A1AA"));
        assert_eq!(read.model.as_deref(), Some("WS-C2960X-24TS-LL"));
        assert!(!read.from_boards);
    }

    /// The C3750E stack measured: a stack row at 1 with no model, a chassis row per member. Two
    /// members of the same model are listed twice, so the i-th model is the i-th serial's.
    #[test]
    fn a_cisco_stack_lists_each_members_model_in_the_serials_order() {
        let read = entity_of(&[
            (1, 11, "", ""),
            (2001, 3, "FDO0000B2BB", "WS-C3750X-24T-E"),
            (1001, 3, "FDO0000A1AA", "WS-C3750X-24T-E"),
        ]);
        assert_eq!(read.serial.as_deref(), Some("FDO0000A1AA, FDO0000B2BB"));
        assert_eq!(
            read.model.as_deref(),
            Some("WS-C3750X-24T-E, WS-C3750X-24T-E")
        );
    }

    /// Decision 30: a member whose serial is empty, or repeats one already listed, is dropped from
    /// both lists at the same position — the model list never slides one member along.
    #[test]
    fn a_member_dropped_from_the_serials_is_dropped_from_the_models() {
        let read = entity_of(&[
            (1001, 3, "SN-A", "MODEL-A"),
            (2001, 3, "", "MODEL-B"),
            (3001, 3, "SN-A", "MODEL-C"),
            (4001, 3, "SN-D", "MODEL-D"),
        ]);
        assert_eq!(read.serial.as_deref(), Some("SN-A, SN-D"));
        assert_eq!(read.model.as_deref(), Some("MODEL-A, MODEL-D"));
    }

    /// A listed member with no model makes the whole list unknown rather than a shorter list that
    /// would pair the remaining models with the wrong serials.
    #[test]
    fn one_member_without_a_model_gives_no_model_list() {
        let read = entity_of(&[(1001, 3, "SN-A", "MODEL-A"), (2001, 3, "SN-B", "  ")]);
        assert_eq!(read.serial.as_deref(), Some("SN-A, SN-B"));
        assert_eq!(read.model, None);
    }

    /// Inc.10 decision 37, the shape measured on a Catalyst 4948E (IOS 12.2(54)SG): the chassis
    /// row answers its serial and an empty model; the supervisor module carries the same serial
    /// and the model. Power supplies and transceivers carry models and serials of their own.
    #[test]
    fn an_empty_chassis_model_is_taken_from_the_module_with_its_serial() {
        let read = entity_of(&[
            (1, 3, "CAT0000A1AA", ""),
            (2, 5, "", ""),
            (7, 6, "PAC0000B2BB", "PWR-C49E-300AC-R"),
            (1000, 9, "CAT0000A1AA", "WS-C4948E"),
            (1001, 10, "", ""),
            (1053, 10, "AVD0000C3CC", "SFP-10G-SR"),
        ]);
        assert_eq!(read.serial.as_deref(), Some("CAT0000A1AA"));
        assert_eq!(read.model.as_deref(), Some("WS-C4948E"));
    }

    /// Only a module with the chassis's own serial lends its model — a line card with a serial of
    /// its own is another part, and a power supply is not a module at all.
    #[test]
    fn a_module_with_another_serial_lends_no_model() {
        let read = entity_of(&[
            (1, 3, "SN-CHASSIS", ""),
            (7, 6, "SN-CHASSIS", "PWR-C49E-300AC-R"),
            (1000, 9, "SN-CARD", "WS-X4548"),
        ]);
        assert_eq!(read.serial.as_deref(), Some("SN-CHASSIS"));
        assert_eq!(read.model, None);
    }

    /// Two modules with the chassis's serial that name different models are not chosen between.
    #[test]
    fn modules_that_disagree_lend_no_model() {
        let read = entity_of(&[
            (1, 3, "SN-A", ""),
            (1000, 9, "SN-A", "MODEL-X"),
            (2000, 9, "SN-A", "MODEL-Y"),
        ]);
        assert_eq!(read.model, None);
        let agreeing = entity_of(&[
            (1, 3, "SN-A", ""),
            (1000, 9, "SN-A", "MODEL-X"),
            (2000, 9, "SN-A", " MODEL-X "),
        ]);
        assert_eq!(agreeing.model.as_deref(), Some("MODEL-X"));
    }

    /// A chassis that answers a model keeps it, and a stack fills only the member that needs it —
    /// the list stays in the serials' order.
    #[test]
    fn a_chassis_model_of_its_own_wins_and_each_member_is_filled_alone() {
        let read = entity_of(&[
            (1, 3, "SN-A", "OWN-MODEL"),
            (2, 3, "SN-B", ""),
            (1000, 9, "SN-A", "MODULE-MODEL-A"),
            (2000, 9, "SN-B", "MODULE-MODEL-B"),
        ]);
        assert_eq!(read.serial.as_deref(), Some("SN-A, SN-B"));
        assert_eq!(read.model.as_deref(), Some("OWN-MODEL, MODULE-MODEL-B"));
    }

    /// `entity_of`'s rows with `entPhysicalVendorType` beside them, as the poller walks it on NX-OS.
    fn entity_with_vendor_types(
        entries: &[(u32, i64, &str, &str)],
        vendor_types: &[(u32, &str)],
    ) -> EntityIdentity {
        let classes = entries.iter().map(|(i, c, _, _)| (*i, *c)).collect();
        let serials = entries
            .iter()
            .map(|(i, _, s, _)| (*i, (*s).to_owned()))
            .collect();
        let models = entries
            .iter()
            .map(|(i, _, _, m)| (*i, (*m).to_owned()))
            .collect();
        let vendor_types = vendor_types
            .iter()
            .map(|(i, v)| (*i, (*v).to_owned()))
            .collect();
        resolve_entity(EntityRows {
            classes: &classes,
            serials: &serials,
            names: &BTreeMap::new(),
            parents: &BTreeMap::new(),
            models: &models,
            vendor_types: &vendor_types,
        })
    }

    /// Inc.11, the shape measured on a Nexus 5010 (NX-OS 5.0(2)N2(1)) with ten FEX: the switch's
    /// chassis row at 149 and one per FEX at 998101.., each FEX row answering a description for its
    /// model and `cevChassis 914` for its vendor type. Every chassis is listed, the switch first.
    #[test]
    fn a_nexus_lists_each_fex_by_the_part_its_vendor_type_names() {
        const FEX_DESCRIPTION: &str = "Fabric Extender Module: 48x1GE, 4x10GE";
        let serials: Vec<String> = (0..=10).map(|i| format!("SSI0000A{i:03}")).collect();
        let mut rows = vec![
            (10, 11, "", ""),
            (22, 9, "JAF0000B1BB", "N5K-C5010P-BF"),
            (149, 3, serials[0].as_str(), "N5K-C5010P-BF"),
        ];
        let mut vendor_types = vec![
            (22, "1.3.6.1.4.1.9.12.3.1.9.72.6"),
            (149, "1.3.6.1.4.1.9.12.3.1.3.798"),
        ];
        for (n, serial) in serials.iter().enumerate().skip(1) {
            let index = 998_100 + u32::try_from(n).expect("small");
            rows.push((index, 3, serial.as_str(), FEX_DESCRIPTION));
            vendor_types.push((index, ".1.3.6.1.4.1.9.12.3.1.3.914"));
        }
        let read = entity_with_vendor_types(&rows, &vendor_types);
        assert_eq!(
            read.serial.as_deref(),
            Some(serials.join(SEPARATOR).as_str())
        );
        let mut expected = vec!["N5K-C5010P-BF"];
        expected.extend(std::iter::repeat_n("N2K-C2248TP-1GE", 10));
        assert_eq!(
            read.model.as_deref(),
            Some(expected.join(SEPARATOR).as_str())
        );
        assert!(read
            .model
            .as_deref()
            .is_some_and(|m| m.len() <= MODEL_MAX_CHARS));
    }

    /// A vendor type the FEX table does not list changes nothing — the model column decides.
    #[test]
    fn a_vendor_type_off_the_fex_table_leaves_the_model_column_alone() {
        let read = entity_with_vendor_types(
            &[
                (149, 3, "SN-A", "N5K-C5010P-BF"),
                (998_101, 3, "SN-B", "Some FEX"),
            ],
            &[
                (149, "1.3.6.1.4.1.9.12.3.1.3.798"),
                (998_101, "1.3.6.1.4.1.9.12.3.1.3.99999"),
            ],
        );
        assert_eq!(read.model.as_deref(), Some("N5K-C5010P-BF, Some FEX"));
    }

    /// The table is read by the arc under `cevChassis`, never by a suffix: the same number under
    /// another branch of the vendor-type tree is another part.
    #[test]
    fn a_fex_part_is_named_only_under_cev_chassis() {
        assert_eq!(
            fex_part("1.3.6.1.4.1.9.12.3.1.3.914"),
            Some("N2K-C2248TP-1GE")
        );
        assert_eq!(
            fex_part(" .1.3.6.1.4.1.9.12.3.1.3.914 "),
            Some("N2K-C2248TP-1GE")
        );
        assert_eq!(fex_part("1.3.6.1.4.1.9.12.3.1.9.914"), None);
        assert_eq!(fex_part("1.3.6.1.4.1.9.12.3.1.3.9140"), None);
        assert_eq!(fex_part("1.3.6.1.4.1.9.12.3.1.3.914.1"), None);
        assert_eq!(fex_part("1.3.6.1.4.1.9.12.3.1.3"), None);
        assert_eq!(fex_part(""), None);
    }

    /// Every part is listed once, by an arc listed once, and each looks like a Nexus 2000 part.
    #[test]
    fn the_fex_table_names_each_arc_and_part_once() {
        for (n, (arc, part)) in FEX_PARTS.iter().enumerate() {
            assert!(part.starts_with("N2K-"), "{part}");
            assert!(part.len() <= MODEL_ITEM_MAX_CHARS, "{part}");
            for (other_arc, other_part) in &FEX_PARTS[n + 1..] {
                assert_ne!(arc, other_arc);
                assert_ne!(part, other_part);
            }
        }
    }

    #[test]
    fn only_nx_os_is_walked_for_vendor_types() {
        assert!(reads_vendor_types(Some("1.3.6.1.4.1.9.12.3.1.3.798")));
        assert!(reads_vendor_types(Some(".1.3.6.1.4.1.9.12.3.1.3.1084")));
        assert!(!reads_vendor_types(Some("1.3.6.1.4.1.9.1.1178")), "IOS");
        assert!(!reads_vendor_types(Some("1.3.6.1.4.1.9.12.3.1.30")));
        assert!(!reads_vendor_types(Some("1.3.6.1.4.1.2011.2.23.291")));
        assert!(!reads_vendor_types(None));
    }

    /// A chassis with no serial borrows nothing: there is nothing to match a module by.
    #[test]
    fn a_chassis_without_a_serial_borrows_no_model() {
        let read = entity_of(&[(1, 3, "", ""), (1000, 9, "", "WS-C4948E")]);
        assert_eq!(read.model, None);
    }

    /// A chassis row with a model and no serial still says what the device is.
    #[test]
    fn a_chassis_model_without_a_serial_is_still_shown() {
        let read = entity_of(&[(1, 3, "", "ASAv"), (2, 9, "", "")]);
        assert_eq!(read.serial, None);
        assert_eq!(read.model.as_deref(), Some("ASAv"));
    }

    /// A Huawei VRP stack (S6730 measured) and an AC6508 answer no `entPhysicalModelName`, so
    /// there is no model here and the caller falls back to `sysDescr`. The VRP chassis name is not
    /// a model and is not taken.
    #[test]
    fn a_huawei_without_the_model_column_gives_no_model() {
        let read = HuaweiMaps::flat(&[
            (67_108_867, 3, "HUAWEI S6730 Routing Switch", "SN-0"),
            (67_108_873, 9, "MPU Board 0", "SN-0"),
            (68_157_449, 9, "MPU Board 1", "SN-1"),
        ])
        .entity();
        assert_eq!(read.serial.as_deref(), Some("SN-0, SN-1"));
        assert_eq!(read.model, None);
        let ac = HuaweiMaps::flat(&[(3, 3, "AC6508", ""), (9, 9, "SRU Board 0", "SN-AC")]).entity();
        assert_eq!(ac.serial.as_deref(), Some("SN-AC"));
        assert_eq!(ac.model, None);
    }

    /// Decision 34, the shape of a lab USG6530F-D (2026-11-10): its MPU card sits in `MPU slot 0`
    /// exactly as a CloudEngine member's board does, but the card's model is a part number. The
    /// serial still comes off the board; the model is left to `sysDescr`'s `USG6530F-D`.
    #[test]
    fn a_usg_card_model_is_not_the_firewalls_model() {
        let usg = || {
            HuaweiMaps::new(
                &[
                    (16_777_216, 3, "USG6530F-D", "SN-USG"),
                    (16_777_218, 5, "MPU slot 0", ""),
                    (16_777_217, 9, "MPUA-USG6530F-D 0", "SN-USG"),
                ],
                &[
                    (16_777_216, 0),
                    (16_777_218, 16_777_216),
                    (16_777_217, 16_777_218),
                ],
            )
            .with_models(&[(16_777_216, ""), (16_777_217, "MPUA-USG6530F-D")])
        };
        let read = usg().entity();
        assert!(read.from_boards, "the board shape is still recognised");
        assert_eq!(read.serial.as_deref(), Some("SN-USG"));
        assert_eq!(read.model, None);

        // The same rows under a CloudEngine chassis name take the board's model.
        let mut cloudengine = usg();
        cloudengine
            .names
            .insert(16_777_216, "CloudEngine S5735-L-V2".to_owned());
        assert_eq!(
            cloudengine.entity().model.as_deref(),
            Some("MPUA-USG6530F-D")
        );
    }

    /// The shape of a PoC two-member CloudEngine S5735-L-V2 stack on YunShan V600R022 (walked
    /// 2026-11-11): the row indexes and names are the device's, the serials are made up. That
    /// release answers No Such Object for the whole model column, so `models` is empty.
    fn r022_stack() -> HuaweiMaps {
        HuaweiMaps::new(
            &[
                (16_777_216, 3, "CloudEngine S5735-L-V2", "SN-M1"),
                (16_842_752, 5, "MPU slot 1", ""),
                (16_842_753, 9, "S5735-L24T4XE-A-V2 1", "SN-M1"),
                (16_908_288, 5, "MPU slot 2", ""),
                (16_908_289, 9, "S5735-L24T4XE-A-V2 2", "SN-M2"),
            ],
            &[
                (16_777_216, 0),
                (16_842_752, 16_777_216),
                (16_842_753, 16_842_752),
                (16_908_288, 16_777_216),
                (16_908_289, 16_908_288),
            ],
        )
    }

    /// Inc.9 decision 36: with no model column, each member's model is its board's name with the
    /// member number taken off, in the serials' order.
    #[test]
    fn a_cloudengine_with_no_model_column_takes_each_model_from_its_board_name() {
        let read = r022_stack().entity();
        assert!(read.from_boards);
        assert_eq!(read.serial.as_deref(), Some("SN-M1, SN-M2"));
        assert_eq!(
            read.model.as_deref(),
            Some("S5735-L24T4XE-A-V2, S5735-L24T4XE-A-V2")
        );
    }

    /// Decision 36 is decided board by board: a board that answers a model keeps it, and only the
    /// one that does not falls back to its name.
    #[test]
    fn a_board_that_answers_a_model_keeps_it_beside_one_read_from_its_name() {
        let read = r022_stack()
            .with_models(&[(16_842_753, "S5735-L24ST4XE-A-V2"), (16_908_289, "  ")])
            .entity();
        assert_eq!(
            read.model.as_deref(),
            Some("S5735-L24ST4XE-A-V2, S5735-L24T4XE-A-V2")
        );
    }

    /// Decision 34 still gates decision 36: the same rows under a USG's chassis name give no model,
    /// so `sysDescr`'s answer stands.
    #[test]
    fn a_board_name_is_not_a_model_off_a_cloudengine() {
        let mut usg = r022_stack();
        usg.names.insert(16_777_216, "USG6530F-D".to_owned());
        let read = usg.entity();
        assert_eq!(read.serial.as_deref(), Some("SN-M1, SN-M2"));
        assert_eq!(read.model, None);
    }

    /// A board named for its slot (`MPU Board N`, decision 18) says nothing about its model, even
    /// under a CloudEngine chassis name, so no model list is made.
    #[test]
    fn an_mpu_board_name_is_never_a_model() {
        let read = HuaweiMaps::flat(&[
            (1, 3, "CloudEngine S5735-L-V2", ""),
            (9, 9, "MPU Board 0", "SN-0"),
            (10, 9, "MPU Board 1", "SN-1"),
        ])
        .entity();
        assert!(read.from_boards);
        assert_eq!(read.serial.as_deref(), Some("SN-0, SN-1"));
        assert_eq!(read.model, None);
    }

    /// No class column: the lone-serial rule still answers the serial, and no row is the device,
    /// so no model is guessed.
    #[test]
    fn with_no_class_column_there_is_no_model() {
        let serials = BTreeMap::from([(1, "SN-A".to_owned())]);
        let models = BTreeMap::from([(1, "AIR-CT3504-K9".to_owned())]);
        let read = resolve_entity(EntityRows {
            classes: &BTreeMap::new(),
            serials: &serials,
            names: &BTreeMap::new(),
            parents: &BTreeMap::new(),
            models: &models,
            vendor_types: &BTreeMap::new(),
        });
        assert_eq!(read.serial.as_deref(), Some("SN-A"));
        assert_eq!(read.model, None);
    }

    /// The model cap fits the most members listed at the longest model each, so a stack's model
    /// list is never cut while its serial list is not.
    #[test]
    fn the_longest_model_list_fits_the_model_cap() {
        let item = "M".repeat(MODEL_ITEM_MAX_CHARS);
        let list = [item.as_str(); SERIAL_MAX_CHASSIS].join(SEPARATOR);
        assert_eq!(sanitize_model(&list).as_deref(), Some(list.as_str()));
        let long = "M".repeat(MODEL_ITEM_MAX_CHARS + 10);
        assert_eq!(
            sanitize_model_item(&long).map(|m| m.chars().count()),
            Some(MODEL_ITEM_MAX_CHARS)
        );
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
