// SPDX-License-Identifier: AGPL-3.0-only
//! Interface media type: `ifMauTable` and the ENTITY-MIB fallback (ADR-063 Inc.2).
//!
//! Pure functions over already-walked rows, the same split `optical.rs` uses — the SNMP sessions
//! live in [`crate::worker`] and everything worth testing lives here. What can go wrong quietly:
//!
//! - **the index**, because `ifMauTable` is keyed by `(ifMauIfIndex, ifMauIndex)` and the ordinary
//!   walkers fold a multi-subid tail into a hash. Reading the ifIndex out of the *first*
//!   sub-identifier is the whole reason this check exists separately;
//! - **which row wins** when a port has several MAUs, because any answer is plausible and only one
//!   is predictable;
//! - **the fallback**, because ENTITY-MIB answers with a part number and calling that a media type
//!   would be a confident lie — the failure mode `optical.rs`'s header warns about.
//!
//! ⚠️ **Unexercised against a device that implements MAU-MIB.** The one SNMP device in this
//! project's lab does not answer EtherLike-MIB, which is more widely implemented than MAU-MIB, so
//! it is not expected to answer this either. Every guard below therefore drops rather than guesses.

use std::collections::BTreeMap;

/// Media rows recovered from CISCO-STACK-MIB, keyed by ifIndex (ADR-063 Inc.7).
///
/// Separate from [`media_by_ifindex`] because the index is different in kind, not just in shape:
/// `portTable` is keyed by `(portModuleIndex, portIndex)` — a physical slot/port coordinate that
/// has no arithmetic relationship to ifIndex — and carries its own translation column. Folding the
/// two walks into one function would mean one of them silently reading the other's index.
///
/// `speed_oid` is `ifHighSpeed`, walked here rather than borrowed from the interface job: three of
/// the `portType` values state a port's *capability* (a 10/100/1000 socket) rather than its
/// negotiated rate, and BASE-T's designation is a function of the rate. Fibre readings answer
/// without it, so a port whose speed the device never reported still gets a medium.
#[must_use]
pub fn cisco_media_by_ifindex(
    rows: &[SnmpInstanceRow],
    type_oid: &str,
    ifindex_oid: &str,
    speed_oid: &str,
) -> BTreeMap<u32, String> {
    let int_of = |row: &SnmpInstanceRow| match &row.value {
        SnmpValue::Int(v) => Some(*v),
        _ => None,
    };
    // (module, port) -> ifIndex. The instance is the port's coordinate; the *value* is the ifIndex.
    let mut by_port: BTreeMap<Vec<u32>, u32> = BTreeMap::new();
    for row in rows.iter().filter(|r| r.oid_base == ifindex_oid) {
        if let Some(v) = int_of(row) {
            if let Ok(ifindex) = u32::try_from(v) {
                by_port.insert(row.instance.clone(), ifindex);
            }
        }
    }
    // ifIndex -> bits/sec, from ifHighSpeed's megabits.
    let mut speed: BTreeMap<u32, i64> = BTreeMap::new();
    for row in rows.iter().filter(|r| r.oid_base == speed_oid) {
        if let (Some(&ifindex), Some(mbps)) = (row.instance.first(), int_of(row)) {
            if mbps > 0 {
                speed.insert(ifindex, mbps.saturating_mul(1_000_000));
            }
        }
    }

    let mut out = BTreeMap::new();
    for row in rows.iter().filter(|r| r.oid_base == type_oid) {
        let (Some(&ifindex), Some(code)) = (by_port.get(&row.instance), int_of(row)) else {
            // No translation for this coordinate — a port the device lists in portTable but not in
            // ifTable. There is no interface to attach it to.
            continue;
        };
        #[allow(clippy::cast_precision_loss)]
        if let Some(media) =
            yagra_common::media_from_cisco_port_type(code as f64, speed.get(&ifindex).copied())
        {
            out.insert(ifindex, media.to_owned());
        }
    }
    out
}

/// `entPhysicalClass` value for `sensor(8)` — the one class that can never be a transceiver, and
/// the one that produced a convincing-looking wrong answer on real hardware.
const ENT_CLASS_SENSOR: i64 = 8;
use yagra_common::{mau_subid, media_from_mau_oid, media_from_transceiver_text, Duplex};
use yagra_transport::{SnmpInstanceRow, SnmpValue};

/// What one poll learned about one port's physical media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRow {
    /// Canonical IEEE designation, e.g. `1000BASE-T`.
    pub media: Option<String>,
    /// The duplex the MAU registration encoded, where it encoded one.
    ///
    /// **Secondary to `dot3StatsDuplexStatus`**, which the interface walk reads on the fast path.
    /// Core's upsert COALESCEs, so this fills the column only where the primary left it NULL —
    /// which on a device with MAU-MIB but no EtherLike-MIB is every port.
    pub duplex: Option<Duplex>,
    /// The pluggable's vendor part string, verbatim, when ENTITY-MIB supplied one.
    pub transceiver_model: Option<String>,
}

/// `ifMauType` rows → one media row per ifIndex. Rows of any other column are ignored, which is
/// what lets this share a walk with the Cisco `portTable` columns (ADR-110 Increment 4).
///
/// The ifIndex is the **first** sub-identifier of the instance (`ifMauIfIndex`); the second is
/// `ifMauIndex`, which distinguishes several MAUs on one port.
///
/// **First row per ifIndex wins, in ascending index order** — the same stated, checkable rule
/// `optical::dedupe_readings` applies to QSFP lanes, and for the same reason: no aggregate over
/// several MAUs is both correct and unsurprising, so the rule that can be written down beats the
/// rule that looks clever. Walk order is the agent's ascending order, so this is `ifMauIndex 1`.
///
/// A value the registry table does not carry is skipped and its sub-identifier returned in
/// `unknown_subids`, so the caller can log a *number* — which is what someone extending the table
/// needs. It is never guessed at.
#[must_use]
pub fn media_by_ifindex(
    rows: &[SnmpInstanceRow],
    mau_type_oid: &str,
) -> (BTreeMap<u32, MediaRow>, Vec<u32>) {
    let mut out: BTreeMap<u32, MediaRow> = BTreeMap::new();
    let mut unknown: Vec<u32> = Vec::new();
    // Filtered by column, not by value shape. Since ADR-110 Increment 4 the caller asks for
    // `ifMauType` and the three Cisco `portTable` columns in **one** walk, so `rows` carries
    // both families; leaning on "only `ifMauType` is an OBJECT IDENTIFIER" would be relying
    // on a coincidence of value types rather than on what was asked for.
    for row in rows.iter().filter(|r| r.oid_base == mau_type_oid) {
        let Some(&ifindex) = row.instance.first() else {
            continue;
        };
        let SnmpValue::Oid(value) = &row.value else {
            // `ifMauType` is an OBJECT IDENTIFIER. Anything else is a device answering a shape the
            // MIB does not define, and there is nothing to recover from it.
            continue;
        };
        let Some(found) = media_from_mau_oid(value) else {
            if let Some(subid) = mau_subid(value) {
                if !unknown.contains(&subid) {
                    unknown.push(subid);
                }
            }
            continue;
        };
        out.entry(ifindex).or_insert_with(|| MediaRow {
            media: Some(found.media.to_owned()),
            duplex: found.duplex,
            transceiver_model: None,
        });
    }
    (out, unknown)
}

/// The best describing string per entity — its model name or description, but **never a restatement
/// of its own name**.
///
/// `name_oid` is `entPhysicalName`; every other column in `rows` is treated as a candidate
/// description. Device text is untrusted and not guaranteed UTF-8 (`security.md`), so it is decoded
/// lossily rather than dropped — a part string with one bad byte still answers "what module is in
/// there", and it is escaped before rendering like every other device-supplied string.
///
/// 🚨 **Dropping a candidate that equals the entity's own name is the load-bearing rule, and it was
/// found on real hardware after shipping without it.** ENTITY-MIB describes *every* component, and
/// the entity a port's ifIndex resolves to is usually the **port itself**, not a transceiver inside
/// it. On the Huawei USG measured here, every port entity's description is simply the port name, so
/// the first version of this stored `GE0/0/1` as `GE0/0/1`'s transceiver model — rendering
/// "Transceiver: GE0/0/1" at the operator. A description that repeats the component's own name
/// carries no information, and presenting it as a part number is precisely the confident lie this
/// module's header warns about.
///
/// Comparison is case- and whitespace-insensitive: a vendor that writes `GE0/0/1` in one column and
/// `ge0/0/1 ` in the other is saying the same nothing twice.
/// 🚨 **A second rule was needed, and only a live deployment showed why.** Dropping a description
/// that equals the entity's own name is not enough: most devices describe the *port* with a string
/// that is merely *different* from its name and still says nothing about a module. Measured on the
/// running deployment, `transceiver_model` had been filled with `"Linecard-Port"` on 54 Nexus
/// ports, `"Port"` on 53 Huawei ports, `"Ethernet Port, Vitual Domain: root"` on 47 FortiGate
/// ports, `"N/A"` and — worst — `"Transceiver Rx Power Sensor"`, which is a **sensor's** name, on
/// IOS-XR. An operator reading that column was being told a part number that does not exist.
///
/// The guard is [`ENT_PHYSICAL_IS_FRU`]: a pluggable is field-replaceable, a soldered-down port is
/// not. Measured across eight captures it separates them 7/8 —
///
/// | device | isFRU | text | verdict |
/// |---|---|---|---|
/// | c9500X / 2960X / c9400 | true | `SFP-H10GB-CU1M`, `GLC-SX-MMD`, `SFP-10G-AOC2M` | kept ✓ |
/// | N9K / IOS-XR / FortiGate / S5720 | false | `Linecard-Port`, `N/A`, `Port` | dropped ✓ |
/// | NE8000 | false | `10000Mb/s-1200nm-Copper Pigtail-10m` | a **real** module marked not-FRU ✗ |
///
/// ⚠️ So `isFRU = false` is **not** proof of "no module" — one vendor gets it wrong — and rejecting
/// on it alone would lose a genuine part string. The second half of the rule catches that case: a
/// part number or a rate always contains a **digit**, and every noise string measured above contains
/// none. Both halves are needed; either alone is wrong on real data.
#[must_use]
pub fn entity_text(
    rows: &[SnmpInstanceRow],
    name_oid: &str,
    fru_oid: &str,
    class_oid: &str,
) -> BTreeMap<u32, String> {
    let read = |row: &SnmpInstanceRow| -> Option<(u32, String)> {
        let &ent = row.instance.first()?;
        let SnmpValue::Bytes(bytes) = &row.value else {
            return None;
        };
        let text = String::from_utf8_lossy(bytes).trim().to_owned();
        (!text.is_empty()).then_some((ent, text))
    };
    let fold = |s: &str| s.trim().to_ascii_lowercase();

    // 🚨 A **sensor** is never a module, and one slipped past the two rules above on the first live
    // run: an IOS-XR router attaches `Transceiver Voltage Sensor - 3.3V` to 13 of its ports, and
    // "3.3V" is a digit, so the digit clause admitted it. The word "Transceiver" in it makes it read
    // convincingly, which is exactly what makes it worth excluding structurally rather than by text:
    // `entPhysicalClass` says `sensor(8)` and no string rule has to guess.
    let mut sensors: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for row in rows.iter().filter(|r| r.oid_base == class_oid) {
        if let (Some(&ent), SnmpValue::Int(ENT_CLASS_SENSOR)) = (row.instance.first(), &row.value) {
            sensors.insert(ent);
        }
    }

    // `entPhysicalIsFRU`: true(1) / false(2). Absent is treated as "not stated", which falls to the
    // digit rule rather than to either extreme.
    let mut is_fru: BTreeMap<u32, bool> = BTreeMap::new();
    for row in rows.iter().filter(|r| r.oid_base == fru_oid) {
        if let (Some(&ent), SnmpValue::Int(v)) = (row.instance.first(), &row.value) {
            is_fru.insert(ent, *v == 1);
        }
    }

    let mut names: BTreeMap<u32, String> = BTreeMap::new();
    for row in rows.iter().filter(|r| r.oid_base == name_oid) {
        if let Some((ent, text)) = read(row) {
            names.entry(ent).or_insert(text);
        }
    }

    let mut out: BTreeMap<u32, String> = BTreeMap::new();
    for row in rows
        .iter()
        .filter(|r| r.oid_base != name_oid && r.oid_base != fru_oid && r.oid_base != class_oid)
    {
        let Some((ent, text)) = read(row) else {
            continue;
        };
        if sensors.contains(&ent) {
            continue;
        }
        if names.get(&ent).is_some_and(|n| fold(n) == fold(&text)) {
            continue;
        }
        // See the 🚨 above: field-replaceable, or bearing a digit. Neither test alone survives the
        // measured data.
        if !is_fru.get(&ent).copied().unwrap_or(false) && !text.chars().any(|c| c.is_ascii_digit())
        {
            continue;
        }
        // Keep the longer candidate: `entPhysicalModelName` is usually the part number and
        // `entPhysicalDescr` the prose, and which one carries a designation varies by vendor. The
        // matcher downstream refuses anything it cannot recognise anyway.
        match out.get(&ent) {
            Some(existing) if existing.len() >= text.len() => {}
            _ => {
                out.insert(ent, text);
            }
        }
    }
    out
}

/// Fold ENTITY-MIB transceiver strings into rows MAU did not already answer.
///
/// `resolve_ifindex` is `EntityIndex::ifindex_for` — passed in rather than imported so this stays a
/// pure function the tests can drive without building an index.
///
/// Two rules, and the second is the one that matters:
/// - **MAU wins.** A port `ifMauTable` already answered for is left completely alone; this cannot
///   overwrite a registry designation with a part number.
/// - **The part string is always kept, the media only sometimes.** `transceiver_model` records what
///   the device said, verbatim. `media` is filled only when that string genuinely contains a
///   canonical designation — see `media_from_transceiver_text`, which refuses when unsure.
pub fn merge_entity_fallback(
    out: &mut BTreeMap<u32, MediaRow>,
    text_by_entity: &BTreeMap<u32, String>,
    mut resolve_ifindex: impl FnMut(u32) -> Option<u32>,
) {
    for (&ent, text) in text_by_entity {
        let Some(ifindex) = resolve_ifindex(ent) else {
            // Nothing in the containment chain maps to an interface — a fan tray, a power supply,
            // or a device with no alias table. Dropping is right: there is no port to attach it to.
            continue;
        };
        if out.contains_key(&ifindex) {
            continue;
        }
        let media = media_from_transceiver_text(text).map(str::to_owned);
        if media.is_none() && text.is_empty() {
            continue;
        }
        out.insert(
            ifindex,
            MediaRow {
                media,
                duplex: None,
                transceiver_model: Some(text.clone()),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mau_row(ifindex: u32, mau_index: u32, subid: u32) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: yagra_common::OID_IF_MAU_TYPE.to_owned(),
            instance: vec![ifindex, mau_index],
            value: SnmpValue::Oid(format!("1.3.6.1.2.1.26.4.{subid}")),
        }
    }

    /// `entPhysicalName` — the yardstick column, not a candidate.
    const NAME_OID: &str = "1.3.6.1.2.1.47.1.1.1.1.7";
    /// `entPhysicalModelName` — a candidate.
    const MODEL_OID: &str = "1.3.6.1.2.1.47.1.1.1.1.13";
    /// `entPhysicalIsFRU` — walked beside the describing columns, and a yardstick like NAME_OID.
    const FRU_OID: &str = "1.3.6.1.2.1.47.1.1.1.1.16";
    /// `entPhysicalClass` — the third yardstick.
    const CLASS_OID: &str = "1.3.6.1.2.1.47.1.1.1.1.5";

    fn class_row(ent: u32, class: i64) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: CLASS_OID.to_owned(),
            instance: vec![ent],
            value: SnmpValue::Int(class),
        }
    }

    fn fru_row(ent: u32, yes: bool) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: FRU_OID.to_owned(),
            instance: vec![ent],
            value: SnmpValue::Int(if yes { 1 } else { 2 }),
        }
    }

    fn text_row(ent: u32, text: &str) -> SnmpInstanceRow {
        row_on(MODEL_OID, ent, text)
    }

    fn row_on(oid: &str, ent: u32, text: &str) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: oid.to_owned(),
            instance: vec![ent],
            value: SnmpValue::Bytes(text.as_bytes().to_vec()),
        }
    }

    #[test]
    fn the_ifindex_comes_from_the_first_subid_not_a_folded_hash() {
        // The reason this check exists at all. If the two-subid instance were ever collapsed the
        // way the ordinary walkers collapse it, every row would land on a synthetic key and the
        // column would be empty on every device — silently.
        let (got, unknown) = media_by_ifindex(
            &[mau_row(7, 1, 30), mau_row(8, 1, 26)],
            yagra_common::OID_IF_MAU_TYPE,
        );
        assert!(unknown.is_empty());
        assert_eq!(got[&7].media.as_deref(), Some("1000BASE-T"));
        assert_eq!(got[&7].duplex, Some(Duplex::Full));
        assert_eq!(got[&8].media.as_deref(), Some("1000BASE-SX"));
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn the_first_mau_per_port_wins() {
        // A stated rule beats a clever one. Ascending `ifMauIndex` is the agent's own walk order.
        let (got, _) = media_by_ifindex(
            &[mau_row(7, 1, 30), mau_row(7, 2, 26)],
            yagra_common::OID_IF_MAU_TYPE,
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[&7].media.as_deref(), Some("1000BASE-T"));
    }

    #[test]
    fn an_unrecognised_registration_is_reported_as_a_number_not_stored() {
        // 154 is an IEEE 802.3ca EPON registration the table deliberately omits rather than render
        // with a rule that could not be trusted for it. ⚠️ This test used to name 103, which meant
        // "past the transcribed block" until the table was generated from the registry — 103 is
        // 2.5GBASE-T and is now perfectly well known. The example has to be a **deliberate** gap,
        // not merely a high number, or the test decays into "some number we have not reached yet".
        let (got, unknown) = media_by_ifindex(
            &[mau_row(7, 1, 154), mau_row(8, 1, 30)],
            yagra_common::OID_IF_MAU_TYPE,
        );
        assert!(!got.contains_key(&7), "must not guess a medium");
        assert_eq!(unknown, vec![154]);
        assert_eq!(got[&8].media.as_deref(), Some("1000BASE-T"));
    }

    const CISCO_TYPE: &str = "1.3.6.1.4.1.9.5.1.4.1.1.5";
    const CISCO_IFX: &str = "1.3.6.1.4.1.9.5.1.4.1.1.11";
    const HIGH_SPEED: &str = "1.3.6.1.2.1.31.1.1.1.15";

    fn port_row(oid: &str, module: u32, port: u32, v: i64) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: oid.to_owned(),
            instance: vec![module, port],
            value: SnmpValue::Int(v),
        }
    }
    fn speed_row(ifindex: u32, mbps: i64) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: HIGH_SPEED.to_owned(),
            instance: vec![ifindex],
            value: SnmpValue::Int(mbps),
        }
    }

    /// 🚨 The index is a slot/port coordinate, not an ifIndex, and the table carries the map.
    ///
    /// Reading `portType`'s instance as an ifIndex would attach every answer to the wrong
    /// interface — on the lab 2960X, port `(1,1)` is ifIndex **10101**, so the mistake would be
    /// silent and total rather than obviously broken.
    #[test]
    fn a_cisco_port_type_reaches_its_interface_through_the_tables_own_map() {
        let got = cisco_media_by_ifindex(
            &[
                // (module 1, port 1) is ifIndex 10101 — a 10/100/1000 copper socket at 1 Gbit/s.
                port_row(CISCO_IFX, 1, 1, 10101),
                port_row(CISCO_TYPE, 1, 1, 61),
                speed_row(10101, 1000),
                // (1,52) is ifIndex 10152 — a 1000BASE-SX optic. Fibre answers without a speed.
                port_row(CISCO_IFX, 1, 52, 10152),
                port_row(CISCO_TYPE, 1, 52, 28),
            ],
            CISCO_TYPE,
            CISCO_IFX,
            HIGH_SPEED,
        );
        assert_eq!(got[&10101], "1000BASE-T");
        assert_eq!(got[&10152], "1000BASE-SX");
        assert!(
            !got.contains_key(&1),
            "the raw port coordinate is never an ifIndex"
        );
        assert_eq!(got.len(), 2);
    }

    /// The same copper socket, two rates, two correct answers — and no rate, no answer.
    #[test]
    fn a_copper_capability_follows_the_ports_actual_rate() {
        let rows = |mbps: Option<i64>| {
            let mut v = vec![
                port_row(CISCO_IFX, 1, 1, 10101),
                port_row(CISCO_TYPE, 1, 1, 18),
            ];
            if let Some(m) = mbps {
                v.push(speed_row(10101, m));
            }
            v
        };
        let at = |mbps| {
            cisco_media_by_ifindex(&rows(mbps), CISCO_TYPE, CISCO_IFX, HIGH_SPEED)
                .get(&10101)
                .cloned()
        };
        assert_eq!(at(Some(100)).as_deref(), Some("100BASE-TX"));
        assert_eq!(at(Some(1000)).as_deref(), Some("1000BASE-T"));
        assert_eq!(
            at(None),
            None,
            "a capability with no rate is half an answer"
        );
        assert_eq!(
            at(Some(0)),
            None,
            "a zero rate is 'never advertised', not 0 bps"
        );
    }

    #[test]
    fn a_port_with_no_ifindex_translation_or_no_medium_is_dropped() {
        let got = cisco_media_by_ifindex(
            &[
                // portType with no matching portIfIndex row — nothing to attach it to.
                port_row(CISCO_TYPE, 9, 9, 28),
                // An empty cage: e1000Empty(31) is "no GBIC installed", not a medium.
                port_row(CISCO_IFX, 1, 2, 10102),
                port_row(CISCO_TYPE, 1, 2, 31),
                // Not Ethernet at all.
                port_row(CISCO_IFX, 1, 3, 10103),
                port_row(CISCO_TYPE, 1, 3, 22),
            ],
            CISCO_TYPE,
            CISCO_IFX,
            HIGH_SPEED,
        );
        assert!(got.is_empty(), "nothing here names a medium: {got:?}");
    }

    #[test]
    fn a_non_oid_value_and_an_empty_instance_are_dropped() {
        let rows = vec![
            SnmpInstanceRow {
                oid_base: yagra_common::OID_IF_MAU_TYPE.to_owned(),
                instance: vec![],
                value: SnmpValue::Oid("1.3.6.1.2.1.26.4.30".to_owned()),
            },
            SnmpInstanceRow {
                oid_base: yagra_common::OID_IF_MAU_TYPE.to_owned(),
                instance: vec![9, 1],
                value: SnmpValue::Int(30),
            },
        ];
        let (got, unknown) = media_by_ifindex(&rows, yagra_common::OID_IF_MAU_TYPE);
        assert!(got.is_empty());
        assert!(unknown.is_empty());
    }

    #[test]
    fn entity_text_keeps_the_longer_of_the_candidate_columns() {
        let got = entity_text(
            &[
                text_row(101, "SFP"),
                text_row(101, "SFP-1000BaseLX transceiver"),
                text_row(102, "  "),
            ],
            NAME_OID,
            FRU_OID,
            CLASS_OID,
        );
        assert_eq!(got[&101], "SFP-1000BaseLX transceiver");
        assert!(!got.contains_key(&102), "blank text is not an answer");
    }

    /// 🚨 The defect that reached the test server: a description that is just the port's own name.
    ///
    /// ENTITY-MIB describes every component, and the entity a port's ifIndex resolves to is usually
    /// the **port**, not a module inside it. The Huawei USG answers `entPhysicalDescr` with the port
    /// name, so without this rule every port was reported as its own transceiver and the UI rendered
    /// "Transceiver: GE0/0/1". Nothing in the unit suite caught it — only reading the real table did.
    #[test]
    fn a_description_that_only_restates_the_entity_name_is_not_a_transceiver() {
        let got = entity_text(
            &[
                row_on(NAME_OID, 101, "GE0/0/1"),
                row_on(MODEL_OID, 101, "GE0/0/1"),
                // Case and stray whitespace are the same nothing said twice.
                row_on(NAME_OID, 102, "GE0/0/2"),
                row_on(MODEL_OID, 102, " ge0/0/2 "),
                // …but a real part number on a named port survives, which is the half that would be
                // lost by "just drop everything when a name exists".
                row_on(NAME_OID, 103, "GE0/0/3"),
                row_on(MODEL_OID, 103, "SFP-1000BaseLX"),
            ],
            NAME_OID,
            FRU_OID,
            CLASS_OID,
        );
        assert!(!got.contains_key(&101), "port name is not a part number");
        assert!(!got.contains_key(&102), "case/space folded comparison");
        assert_eq!(got[&103], "SFP-1000BaseLX");
    }

    /// 🚨 The second defect a live deployment showed, one increment after the first.
    ///
    /// These are the exact strings the running box was storing as transceiver models. None is a
    /// part number; the last is a **sensor's** name. Every one is *different* from the port's own
    /// name, so the earlier rule let all of them through.
    #[test]
    fn a_description_of_the_port_is_not_a_module_even_when_it_differs_from_the_name() {
        let noise = [
            (201u32, "Linecard-1 Port-1", "Linecard-Port"), // Nexus, 54 ports
            (202, "GigabitEthernet0/0/1", "Port"),          // Huawei S5720, 53 ports
            (203, "mgmt1", "Ethernet Port, Vitual Domain: root"), // FortiGate, 47 ports
            (204, "TenGigE0/0/0/1", "N/A"),                 // IOS-XR
            (205, "TenGigE0/7/0/0", "Transceiver Rx Power Sensor"), // IOS-XR: a SENSOR name
        ];
        let mut rows = Vec::new();
        for (ent, name, text) in noise {
            rows.push(row_on(NAME_OID, ent, name));
            rows.push(row_on(MODEL_OID, ent, text));
            rows.push(fru_row(ent, false));
        }
        let got = entity_text(&rows, NAME_OID, FRU_OID, CLASS_OID);
        assert!(
            got.is_empty(),
            "none of these is a part number; got {got:?}",
        );
    }

    /// ⚠️ …and the accepting half, which is what stops the rule above from being "reject
    /// everything". Both of these are real strings from real devices, and each survives by a
    /// *different* clause — which is the point: neither clause alone is enough.
    #[test]
    fn a_replaceable_module_survives_and_so_does_a_part_string_with_a_rate_in_it() {
        let got = entity_text(
            &[
                // Field-replaceable, and no digit anywhere in the text: kept by isFRU alone.
                // Cisco 2960X, measured.
                row_on(NAME_OID, 301, "GigabitEthernet1/0/52"),
                row_on(MODEL_OID, 301, "GLC-SX-MMD"),
                fru_row(301, true),
                // NOT flagged replaceable — the vendor is wrong — but unmistakably a module.
                // Huawei NE8000, measured. Rejecting on isFRU alone would lose this.
                row_on(NAME_OID, 302, "GigabitEthernet0/8/0"),
                row_on(
                    MODEL_OID,
                    302,
                    "10000Mb/s-1200nm-Copper Pigtail-10m(0.05mm)",
                ),
                fru_row(302, false),
                // Replaceable and digit-bearing — the ordinary case. Cisco c9500X, measured.
                row_on(NAME_OID, 303, "FiftyGigE1/0/1"),
                row_on(MODEL_OID, 303, "SFP-H10GB-CU1M"),
                fru_row(303, true),
            ],
            NAME_OID,
            FRU_OID,
            CLASS_OID,
        );
        assert_eq!(got[&301], "GLC-SX-MMD", "kept by isFRU with no digit");
        assert_eq!(
            got[&302], "10000Mb/s-1200nm-Copper Pigtail-10m(0.05mm)",
            "kept by the digit rule despite isFRU=false",
        );
        assert_eq!(got[&303], "SFP-H10GB-CU1M");
    }

    /// The FRU column must never become an answer itself.
    ///
    /// Same trap as `entPhysicalName`: it is walked alongside the describing columns, so without an
    /// explicit exclusion the integer `1` would be read as a candidate part number.
    /// 🚨 The one that got past both earlier rules, found on the first live run after shipping them.
    ///
    /// An IOS-XR router attaches `Transceiver Voltage Sensor - 3.3V` to 13 of its ports. It is not
    /// the port's own name, and "3.3V" is a digit — so the name rule and the digit rule both let it
    /// through, and the word *Transceiver* in it made the result read as correct. The exclusion is
    /// structural: `entPhysicalClass` says `sensor(8)`, so no string has to be interpreted.
    #[test]
    fn a_sensor_is_never_a_transceiver_however_convincing_its_name() {
        let got = entity_text(
            &[
                row_on(NAME_OID, 501, "GigabitEthernet0/7/1/1"),
                row_on(MODEL_OID, 501, "Transceiver Voltage Sensor - 3.3V"),
                class_row(501, 8),
                // A module on the same device, reached the same way, must survive — otherwise this
                // rule is "reject IOS-XR" rather than "reject sensors".
                row_on(NAME_OID, 502, "TenGigE0/0/0/0"),
                row_on(
                    MODEL_OID,
                    502,
                    "1000BASE-LX/LH SFP transceiver module, MMF/SMF",
                ),
                class_row(502, 9),
            ],
            NAME_OID,
            FRU_OID,
            CLASS_OID,
        );
        assert!(!got.contains_key(&501), "a sensor is not a module: {got:?}");
        assert_eq!(
            got[&502], "1000BASE-LX/LH SFP transceiver module, MMF/SMF",
            "a real module on the same device is unaffected",
        );
    }

    /// The class column must never become an answer itself — the same trap as the other two.
    #[test]
    fn the_class_column_is_never_itself_a_candidate() {
        let got = entity_text(
            &[row_on(NAME_OID, 601, "Gi1/0/1"), class_row(601, 10)],
            NAME_OID,
            FRU_OID,
            CLASS_OID,
        );
        assert!(
            got.is_empty(),
            "entPhysicalClass is a yardstick, not a description"
        );
    }

    #[test]
    fn the_fru_column_is_never_itself_a_candidate() {
        let got = entity_text(
            &[row_on(NAME_OID, 401, "Gi1/0/1"), fru_row(401, true)],
            NAME_OID,
            FRU_OID,
            CLASS_OID,
        );
        assert!(got.is_empty(), "isFRU is a yardstick, not a description");
    }

    #[test]
    fn the_name_column_is_never_itself_a_candidate() {
        // Even with no describing column at all, `entPhysicalName` must not become the answer —
        // otherwise the rule above is trivially defeated by a device that only implements .7.
        let got = entity_text(
            &[row_on(NAME_OID, 101, "GE0/0/1")],
            NAME_OID,
            FRU_OID,
            CLASS_OID,
        );
        assert!(got.is_empty());
    }

    #[test]
    fn the_fallback_fills_only_ports_mau_did_not_answer() {
        let mut out = BTreeMap::new();
        out.insert(
            7,
            MediaRow {
                media: Some("1000BASE-T".to_owned()),
                duplex: Some(Duplex::Full),
                transceiver_model: None,
            },
        );
        let text = BTreeMap::from([
            (101, "SFP-1000BaseLX".to_owned()),
            (102, "SFP-1000BaseSX".to_owned()),
        ]);
        // entity 101 -> ifIndex 7 (already answered by MAU), entity 102 -> ifIndex 8 (not).
        merge_entity_fallback(&mut out, &text, |ent| match ent {
            101 => Some(7),
            102 => Some(8),
            _ => None,
        });

        // MAU's answer survives untouched — a part number must never overwrite a registration.
        assert_eq!(out[&7].media.as_deref(), Some("1000BASE-T"));
        assert_eq!(out[&7].transceiver_model, None);
        assert_eq!(out[&8].media.as_deref(), Some("1000BASE-SX"));
        assert_eq!(out[&8].transceiver_model.as_deref(), Some("SFP-1000BaseSX"));
    }

    #[test]
    fn a_part_number_with_no_designation_still_records_the_model() {
        // The half that is easy to get wrong: refusing to guess a medium must not throw away the
        // string. An operator reading "OMXD30000" learns which module is in the port.
        let mut out = BTreeMap::new();
        let text = BTreeMap::from([(101, "OMXD30000".to_owned())]);
        merge_entity_fallback(&mut out, &text, |_| Some(4));
        assert_eq!(out[&4].media, None);
        assert_eq!(out[&4].transceiver_model.as_deref(), Some("OMXD30000"));
    }

    #[test]
    fn an_entity_that_maps_to_no_interface_is_dropped() {
        let mut out = BTreeMap::new();
        let text = BTreeMap::from([(101, "SFP-1000BaseLX".to_owned())]);
        merge_entity_fallback(&mut out, &text, |_| None);
        assert!(out.is_empty(), "a fan tray is not a port");
    }
}
