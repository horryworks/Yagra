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

/// `ifMauType` rows → one media row per ifIndex.
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
pub fn media_by_ifindex(rows: &[SnmpInstanceRow]) -> (BTreeMap<u32, MediaRow>, Vec<u32>) {
    let mut out: BTreeMap<u32, MediaRow> = BTreeMap::new();
    let mut unknown: Vec<u32> = Vec::new();
    for row in rows {
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

/// Longest `entPhysicalName`/`entPhysicalDescr`-style string per entity, decoded lossily.
///
/// Device text is untrusted and not guaranteed UTF-8 (`security.md`), so it is decoded lossily
/// rather than dropped — a part string with one bad byte is still the answer to "what module is in
/// there", and it is escaped before rendering like every other device-supplied string.
#[must_use]
pub fn entity_text(rows: &[SnmpInstanceRow]) -> BTreeMap<u32, String> {
    let mut out: BTreeMap<u32, String> = BTreeMap::new();
    for row in rows {
        let Some(&ent) = row.instance.first() else {
            continue;
        };
        let SnmpValue::Bytes(bytes) = &row.value else {
            continue;
        };
        let text = String::from_utf8_lossy(bytes).trim().to_owned();
        if text.is_empty() {
            continue;
        }
        // Keep the longer of the two columns: `entPhysicalModelName` is usually the part number and
        // `entPhysicalDescr` the prose, and which one carries a designation varies by vendor. The
        // longer string is the one more likely to contain it, and the matcher refuses anything it
        // cannot recognise anyway.
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

    fn text_row(ent: u32, text: &str) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: "1.3.6.1.2.1.47.1.1.1.1.13".to_owned(),
            instance: vec![ent],
            value: SnmpValue::Bytes(text.as_bytes().to_vec()),
        }
    }

    #[test]
    fn the_ifindex_comes_from_the_first_subid_not_a_folded_hash() {
        // The reason this check exists at all. If the two-subid instance were ever collapsed the
        // way the ordinary walkers collapse it, every row would land on a synthetic key and the
        // column would be empty on every device — silently.
        let (got, unknown) = media_by_ifindex(&[mau_row(7, 1, 30), mau_row(8, 1, 26)]);
        assert!(unknown.is_empty());
        assert_eq!(got[&7].media.as_deref(), Some("1000BASE-T"));
        assert_eq!(got[&7].duplex, Some(Duplex::Full));
        assert_eq!(got[&8].media.as_deref(), Some("1000BASE-SX"));
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn the_first_mau_per_port_wins() {
        // A stated rule beats a clever one. Ascending `ifMauIndex` is the agent's own walk order.
        let (got, _) = media_by_ifindex(&[mau_row(7, 1, 30), mau_row(7, 2, 26)]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[&7].media.as_deref(), Some("1000BASE-T"));
    }

    #[test]
    fn an_unrecognised_registration_is_reported_as_a_number_not_stored() {
        // 103 is past the transcribed block. The port gets no media, and the number surfaces so a
        // real gap is discoverable from a running deployment.
        let (got, unknown) = media_by_ifindex(&[mau_row(7, 1, 103), mau_row(8, 1, 30)]);
        assert!(!got.contains_key(&7), "must not guess a medium");
        assert_eq!(unknown, vec![103]);
        assert_eq!(got[&8].media.as_deref(), Some("1000BASE-T"));
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
        let (got, unknown) = media_by_ifindex(&rows);
        assert!(got.is_empty());
        assert!(unknown.is_empty());
    }

    #[test]
    fn entity_text_keeps_the_longer_of_the_two_columns() {
        let got = entity_text(&[
            text_row(101, "SFP"),
            text_row(101, "SFP-1000BaseLX transceiver"),
            text_row(102, "  "),
        ]);
        assert_eq!(got[&101], "SFP-1000BaseLX transceiver");
        assert!(!got.contains_key(&102), "blank text is not an answer");
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
