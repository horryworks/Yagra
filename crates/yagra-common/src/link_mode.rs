// SPDX-License-Identifier: AGPL-3.0-only
//! The physical link's negotiated mode: duplex, and the interface types it applies to (ADR-063).
//!
//! # Why the OIDs live here and not in a `CollectionItem`
//!
//! `dot3StatsDuplexStatus` and `ifType` are walked by the poller as part of the interface-metadata
//! walk it already performs, and they are **not** declared on the wire as extra
//! [`InterfaceField`](crate::collection::InterfaceField) variants.
//!
//! That is a deliberate N/N-1 decision, and the reason is worth keeping next to the constants.
//! `InterfaceField` is a plain serde enum with no `#[serde(other)]`, and it is embedded in
//! `SnmpMetaColumn` → `SnmpTableCheck` → `CheckSpec::SnmpTable` → `JobSpec`. `de_lenient_specs`
//! decodes **per `JobSpec` element** and does not reach inside a spec, so a core that started
//! sending `{"field":"duplex"}` would make every N-1 poller fail to decode the whole `SnmpTable`
//! spec and **drop it** — losing that node's octet counters, oper status, error counters and
//! interface names entirely, silently, until the poller is upgraded. Adding `#[serde(other)]` now
//! cannot help: the tolerance has to exist in the binary that already shipped.
//!
//! The codebase had already solved this once and left the answer in a comment on
//! `InterfaceField::Speed`: `ifHighSpeed` is walked poller-side rather than declared on the wire,
//! "so the bus contract stays unchanged". These constants follow that precedent exactly — the
//! poller appends them to its own walk whenever it is gathering interface metadata at all, and the
//! core→poller wire form is byte-identical to before.

use serde::{Deserialize, Serialize};
use std::fmt;

/// `dot3StatsDuplexStatus` — EtherLike-MIB (RFC 3635), the MAC's current duplex mode.
///
/// Indexed by `dot3StatsIndex`, which the MIB defines to equal `ifIndex` — a single sub-identifier,
/// so the poller's ordinary interface walk carries it without any index translation. INTEGER-valued
/// (`unknown(1)`, `halfDuplex(2)`, `fullDuplex(3)`), so it rides the numeric walk rather than the
/// string one.
pub const OID_DOT3_DUPLEX_STATUS: &str = "1.3.6.1.2.1.10.7.2.1.19";

/// `ifType` — IF-MIB, the interface's IANAifType. ifTable column 3, i.e. two columns away from the
/// `ifSpeed` the same walk already reads, which is why collecting it is free.
pub const OID_IF_TYPE: &str = "1.3.6.1.2.1.2.2.1.3";

/// How a link carries traffic in each direction.
///
/// A closed set: IEEE 802.3 defines no third mode, which is what makes this safe to model as an
/// enum *and* safe to put on the bus. ⚠️ If a third variant ever looked necessary, note the cost
/// before adding it — an N-1 core would fail to decode the whole `DiscoveredInterface`, and
/// therefore the whole `PollResult`, rather than just this field. The right move at that point
/// would be a lenient wrapper, not a new variant.
///
/// Contrast the media type (ADR-063 Inc.2), which is *not* an enum: `dot3MauType` is an IANA
/// registry of 250-and-growing designations whose EN and JA renderings are byte-identical, so an
/// enum there would buy 250-arm matches and 500 locale keys with nothing to translate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Duplex {
    /// One direction at a time (CSMA/CD). Only reachable on copper at 1 Gbit/s and below.
    Half,
    /// Both directions at once. The only mode defined above 1 Gbit/s.
    Full,
}

impl Duplex {
    /// Every variant, for exhaustive iteration in tests and in the API's enum surface.
    pub const ALL: [Self; 2] = [Self::Half, Self::Full];

    /// The token stored in `interfaces.if_duplex` and rendered in the API.
    ///
    /// ⚠️ Must agree with the `#[serde(rename_all = "snake_case")]` tag above — they are produced by
    /// two different mechanisms and nothing but the round-trip test below makes them agree. A
    /// disagreement means rows the writer produces are rows the reader cannot parse.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Half => "half",
            Self::Full => "full",
        }
    }

    /// Parse a stored token. Unknown text is `None` rather than a default — a row written by
    /// something that disagreed with this enum should read as "not known", not as "half".
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "half" => Some(Self::Half),
            "full" => Some(Self::Full),
            _ => None,
        }
    }
}

impl fmt::Display for Duplex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Map a `dot3StatsDuplexStatus` reading to a [`Duplex`].
///
/// `unknown(1)` — and anything outside the enumeration — becomes `None`, i.e. exactly the same
/// stored value as "the MIB is not implemented" and "the port is down so there is no row". The
/// three are indistinguishable to every consumer, and migration 0085 records why that collapse is
/// deliberate rather than lossy-by-accident.
///
/// Takes `f64` because the SNMP numeric walk yields `f64`; values that are not finite or not close
/// to an integer are rejected rather than truncated.
#[must_use]
pub fn duplex_from_dot3(value: f64) -> Option<Duplex> {
    if !value.is_finite() || (value - value.round()).abs() > 1e-6 {
        return None;
    }
    match value.round() as i64 {
        2 => Some(Duplex::Half),
        3 => Some(Duplex::Full),
        _ => None,
    }
}

/// `hwEthernetDuplex` — HUAWEI-PORT-MIB, the same fact as [`OID_DOT3_DUPLEX_STATUS`] for devices
/// that do not implement EtherLike-MIB (ADR-063 Inc.3).
///
/// Indexed by `hwEthernetIfIndex`, which the MIB types as `InterfaceIndex` — a single
/// sub-identifier equal to `ifIndex`, so this rides the ordinary numeric interface walk exactly as
/// the standard column does. No new SNMP session, no new `InterfaceField`, no bus change.
///
/// # Why a second column exists at all
///
/// Huawei's YunShan OS answers **neither** EtherLike-MIB nor MAU-MIB. Walked on the lab USG
/// (`jpmyj01fw01`, 2026-08-18): `1.3.6.1.2.1.10.7` and `1.3.6.1.2.1.26` both return
/// `No Such Object`, so both of ADR-063's existing paths are dead there and the duplex column was
/// permanently blank. Note this is a *platform* split, not a vendor one — the S5720 switch answers
/// `dot3StatsDuplexStatus` for 53 interfaces.
///
/// # How the table was identified without trusting the MIB
///
/// The neighbouring column `.13` (`hwEthernetSpeedSet`, `speed100(3)/speed1000(4)/speed10000(5)`)
/// returned 5,5,4,3,4,… across the USG's twelve physical ports, matching `ifHighSpeed`'s
/// 10000,10000,1000,100,1000,… on every one. A MIB file says what a column *should* be; that
/// agreement is what says the walk is reading the column the file describes.
pub const OID_HW_ETHERNET_DUPLEX: &str = "1.3.6.1.4.1.2011.5.25.157.1.1.1.1.14";

/// Map a `hwEthernetDuplex` reading to a [`Duplex`].
///
/// 🚨 **The enumeration is not the standard one, and `1` is the value that differs.**
/// EtherLike-MIB is `unknown(1) / halfDuplex(2) / fullDuplex(3)`; HUAWEI-PORT-MIB is
/// `full(1) / half(2)` with no `unknown`. Reusing [`duplex_from_dot3`] here would read every
/// full-duplex port as `unknown` and store `NULL` — the column would stay blank exactly as it is
/// today, so **the bug would look like the fix simply not working** rather than like wrong data.
/// `every_mapper_disagrees_where_the_enumerations_disagree` pins the two apart.
///
/// Anything outside `{1, 2}` is `None`, the same collapse [`duplex_from_dot3`] documents.
#[must_use]
pub fn duplex_from_huawei(value: f64) -> Option<Duplex> {
    if !value.is_finite() || (value - value.round()).abs() > 1e-6 {
        return None;
    }
    match value.round() as i64 {
        1 => Some(Duplex::Full),
        2 => Some(Duplex::Half),
        _ => None,
    }
}

/// `hwEthernetPortType` — HUAWEI-PORT-MIB, whether a port is metal or optical (ADR-063 Inc.4).
///
/// `INTEGER {other(1), copper(2), fiber(3)}`, indexed by `hwEthernetIfIndex` — the *same* row as
/// [`OID_HW_ETHERNET_DUPLEX`], two columns away. It therefore rides the numeric interface walk that
/// already reads that column: no new session, no new `InterfaceField`, no bus change.
///
/// # Why a vendor column answers a question the standard MIB was supposed to
///
/// The standard answer is `ifMauType` ([`OID_IF_MAU_TYPE`]), and it is **not available here**:
/// Huawei's YunShan OS returns `No Such Object` for the whole of `1.3.6.1.2.1.26` (walked on the lab
/// USG). The other implemented source, ENTITY-MIB transceiver text, **structurally cannot answer for a
/// metal port** — a fixed RJ45 socket has no pluggable entity to describe, so it only ever names optics.
///
/// A search for a cross-vendor substitute came up empty and the search is worth recording, because the
/// obvious next move is to repeat it. Every ifIndex-keyed column in the Cisco 2960X capture was compared
/// between its two known fibre ports and its 131 copper ports: **no column separates them.** The same
/// held for the c9500X, Arista EOS, Comware and Juniper EX captures. Cisco's
/// `entPhysicalVendorType` looked promising and is not — all 120 of the 2960X's port entities carry
/// one identical value.
pub const OID_HW_ETHERNET_PORT_TYPE: &str = "1.3.6.1.4.1.2011.5.25.157.1.1.1.1.12";

/// What a port is physically made of.
///
/// Deliberately **not** on the bus and **not** stored: it exists only long enough to pick a designation
/// for [`Duplex`]'s neighbouring column. Storing it would be a third spelling of the media fact, and
/// [`copper_designation`]'s output already implies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Medium {
    /// Twisted pair. The one case with a unique designation per speed — see [`copper_designation`].
    Copper,
    /// Optical. Read, and deliberately **not** turned into a designation.
    Fiber,
}

/// Map a `hwEthernetPortType` reading to a [`Medium`].
///
/// `other(1)` is `None`: it is the value the agent gives for a port whose type it cannot state, which
/// is "not known", not a third medium.
#[must_use]
pub fn medium_from_huawei(value: f64) -> Option<Medium> {
    if !value.is_finite() || (value - value.round()).abs() > 1e-6 {
        return None;
    }
    match value.round() as i64 {
        2 => Some(Medium::Copper),
        3 => Some(Medium::Fiber),
        _ => None,
    }
}

/// The IEEE designation of a **copper** Ethernet port running at `bps`.
///
/// # This is a definition, not an inference
///
/// IEEE 802.3 registers exactly one twisted-pair standard per speed, so "copper at 1 Gbit/s" and
/// "1000BASE-T" are two spellings of one fact. Every string returned here is a registration already
/// present in [`MAU_TYPES`] — `every_derived_designation_is_a_real_registration` asserts it — so this
/// produces nothing a device implementing MAU-MIB could contradict.
///
/// 🚨 **That identity is the whole reason fibre is excluded.** `interfaces.if_media` has two writers
/// (this walk and the hourly MAU/ENTITY walk) and core's upsert is `COALESCE(EXCLUDED, existing)`,
/// i.e. **last non-NULL wins** — there is no way to express "only fill what the other left empty". For
/// copper that does not matter, because both writers produce the identical string. For fibre it would:
/// MAU-MIB would say `1000BASE-SX` where the most this function could honestly say is
/// `1000BASE-X`, and the cell would alternate between them on every poll. A column that changes its
/// answer twice a minute is worse than an empty one, so [`Medium::Fiber`] is read and dropped.
///
/// ⚠️ 2.5GBASE-T and 5GBASE-T return `None`. They are real and they are **above [`MAU_TYPES`]'s
/// transcribed block**, and that table's rule is to be extended from the IANA registry rather than from
/// memory. Returning `None` keeps this function's promise — that everything it emits is a checked
/// registration — instead of quietly making it a claim about a document nobody read.
#[must_use]
pub fn copper_designation(bps: i64) -> Option<&'static str> {
    match bps {
        10_000_000 => Some("10BASE-T"),
        100_000_000 => Some("100BASE-TX"),
        1_000_000_000 => Some("1000BASE-T"),
        // Above 10 Gbit/s, "copper" means twinax (`…BASE-CR`), which a fixed RJ45 port is not, so a
        // higher reading is a device saying something this mapping has no business interpreting.
        10_000_000_000 => Some("10GBASE-T"),
        _ => None,
    }
}

/// Coerce an SNMP numeric reading to an `ifType` code, or `None` if it is not a plausible one.
///
/// IANAifType codes are positive and small; the upper bound is a sanity check on a garbage reading,
/// not a claim about the registry's size.
#[must_use]
pub fn if_type_from_snmp(value: f64) -> Option<i32> {
    if !value.is_finite() || (value - value.round()).abs() > 1e-6 {
        return None;
    }
    let code = value.round() as i64;
    (1..=65535).contains(&code).then_some(code as i32)
}

/// `ethernetCsmacd(6)` — every modern Ethernet port, copper or optical, and the only IANAifType a
/// duplex or media reading means anything for.
///
/// Deliberately the **only** code this crate names. The registry has ~300 entries; transcribing it
/// would be a mirror of an IANA document with no reader here, because the question "does a link-mode
/// cell apply to this interface?" is a *rendering* decision and is answered once, in
/// `web/src/components/NodeDetail/linkMode.ts`. The wire carries the raw integer so every consumer —
/// the WebUI, an MCP client — decides from the same number rather than from a boolean this crate
/// computed on their behalf.
pub const IF_TYPE_ETHERNET_CSMACD: i32 = 6;

// ── Media type, from MAU-MIB (ADR-063 Inc.2) ────────────────────────────────────────────────

/// `ifMauType` — MAU-MIB (RFC 3636), the media attachment unit a port is running.
///
/// ⚠️ **Indexed by `(ifMauIfIndex, ifMauIndex)` — two sub-identifiers.** That is why media cannot
/// ride the ordinary interface walk the way duplex does: `yagra_transport`'s `ifindex_from_tail`
/// folds any multi-subid tail into a hash, destroying the ifIndex before the poller sees it. The
/// MAU walk must go through the instance walker, which preserves the whole index.
///
/// Its value is an **OBJECT IDENTIFIER**, not an integer — it points into [`DOT3_MAU_TYPE_ROOT`].
pub const OID_IF_MAU_TYPE: &str = "1.3.6.1.2.1.26.2.1.1.3";

/// `ifMauIfIndex` — the same table's own copy of the ifIndex. INTEGER-valued.
///
/// Worth knowing about even though the instance walker does not need it: it is the one column of
/// `ifMauTable` that a plain numeric probe can read, which makes it the way to ask a device
/// "do you implement MAU-MIB at all?" without being able to read `ifMauType` itself.
pub const OID_IF_MAU_IFINDEX: &str = "1.3.6.1.2.1.26.2.1.1.1";

/// The `dot3MauType` registration arc. An `ifMauType` value is this plus one sub-identifier.
pub const DOT3_MAU_TYPE_ROOT: &str = "1.3.6.1.2.1.26.4";

/// `dot3MauType` registrations: sub-identifier → canonical IEEE designation → the duplex the
/// registration itself encodes.
///
/// **Transcribed by hand from RFC 3636 §5 (1–40), RFC 4836 §5 (41–53) and the IANA-MAU-MIB
/// additions (54–78), on 2026-08-16.** There is nothing automatic that can check this table: it
/// mirrors a document that lives on a remote registry, so the "generate one side from the other"
/// escape hatch `extensibility.md` §2 prefers does not apply. The date is here so the next person
/// knows what it was checked against.
///
/// **The cut at 78 is deliberate and is about accuracy, not effort.** 78 is the end of the block
/// this transcription is confident in (…100GBASE-ER4). IANA has continued past it — 2.5GBASE-T,
/// 5GBASE-T, 25G and the 200/400G forms all live above — and **a wrong designation is far worse
/// than a missing one**: a missing one renders an em dash and logs the number, while a wrong one
/// tells an operator their fibre port is copper. Extend it from the registry when a device needs
/// it; do not extend it from memory.
///
/// The duplex column is a **pure transcription** of what each registration states. Registrations
/// from `10GigBaseX(31)` up carry no HD/FD suffix, because IEEE 802.3 defines no half duplex above
/// 1 Gbit/s — they are `None` here rather than inferred to `Full`, so that this table stays a thing
/// a reviewer can check line-by-line against the RFC. Anything that wants that inference should do
/// it separately, where it can be named and tested.
///
/// Several registrations share one media name and differ only in duplex (`1000BaseTHD(29)` /
/// `1000BaseTFD(30)` are both `1000BASE-T`). That is the point of splitting media and duplex into
/// two columns rather than showing the raw registration name.
const MAU_TYPES: &[(u32, &str, Option<Duplex>)] = &[
    // RFC 3636 §5 — the original MAU-MIB registrations.
    (1, "AUI", None),
    (2, "10BASE5", None),
    (3, "FOIRL", None),
    (4, "10BASE2", None),
    (5, "10BASE-T", None),
    (6, "10BASE-FP", None),
    (7, "10BASE-FB", None),
    (8, "10BASE-FL", None),
    (9, "10BROAD36", None),
    (10, "10BASE-T", Some(Duplex::Half)),
    (11, "10BASE-T", Some(Duplex::Full)),
    (12, "10BASE-FL", Some(Duplex::Half)),
    (13, "10BASE-FL", Some(Duplex::Full)),
    (14, "100BASE-T4", None),
    (15, "100BASE-TX", Some(Duplex::Half)),
    (16, "100BASE-TX", Some(Duplex::Full)),
    (17, "100BASE-FX", Some(Duplex::Half)),
    (18, "100BASE-FX", Some(Duplex::Full)),
    (19, "100BASE-T2", Some(Duplex::Half)),
    (20, "100BASE-T2", Some(Duplex::Full)),
    (21, "1000BASE-X", Some(Duplex::Half)),
    (22, "1000BASE-X", Some(Duplex::Full)),
    (23, "1000BASE-LX", Some(Duplex::Half)),
    (24, "1000BASE-LX", Some(Duplex::Full)),
    (25, "1000BASE-SX", Some(Duplex::Half)),
    (26, "1000BASE-SX", Some(Duplex::Full)),
    (27, "1000BASE-CX", Some(Duplex::Half)),
    (28, "1000BASE-CX", Some(Duplex::Full)),
    (29, "1000BASE-T", Some(Duplex::Half)),
    (30, "1000BASE-T", Some(Duplex::Full)),
    // From here up there is no HD/FD suffix to transcribe — 802.3 defines no half duplex ≥ 10G.
    (31, "10GBASE-X", None),
    (32, "10GBASE-LX4", None),
    (33, "10GBASE-R", None),
    (34, "10GBASE-ER", None),
    (35, "10GBASE-LR", None),
    (36, "10GBASE-SR", None),
    (37, "10GBASE-W", None),
    (38, "10GBASE-EW", None),
    (39, "10GBASE-LW", None),
    (40, "10GBASE-SW", None),
    // RFC 4836 §5 — EFM (802.3ah) and backplane additions.
    (41, "10GBASE-CX4", None),
    (42, "2BASE-TL", None),
    (43, "10PASS-TS", None),
    (44, "100BASE-BX10-D", None),
    (45, "100BASE-BX10-U", None),
    (46, "100BASE-LX10", None),
    (47, "1000BASE-BX10-D", None),
    (48, "1000BASE-BX10-U", None),
    (49, "1000BASE-LX10", None),
    (50, "1000BASE-PX10-D", None),
    (51, "1000BASE-PX10-U", None),
    (52, "1000BASE-PX20-D", None),
    (53, "1000BASE-PX20-U", None),
    // IANA-MAU-MIB additions.
    (54, "10GBASE-T", None),
    (55, "10GBASE-LRM", None),
    (56, "1000BASE-KX", None),
    (57, "10GBASE-KX4", None),
    (58, "10GBASE-KR", None),
    (59, "10/1GBASE-PRX-D1", None),
    (60, "10/1GBASE-PRX-D2", None),
    (61, "10/1GBASE-PRX-D3", None),
    (62, "10/1GBASE-PRX-U1", None),
    (63, "10/1GBASE-PRX-U2", None),
    (64, "10/1GBASE-PRX-U3", None),
    (65, "10GBASE-PR-D1", None),
    (66, "10GBASE-PR-D2", None),
    (67, "10GBASE-PR-D3", None),
    (68, "10GBASE-PR-U1", None),
    (69, "10GBASE-PR-U3", None),
    (70, "40GBASE-KR4", None),
    (71, "40GBASE-CR4", None),
    (72, "40GBASE-SR4", None),
    (73, "40GBASE-FR", None),
    (74, "40GBASE-LR4", None),
    (75, "100GBASE-CR10", None),
    (76, "100GBASE-SR10", None),
    (77, "100GBASE-LR4", None),
    (78, "100GBASE-ER4", None),
];

/// What one `ifMauType` value says about a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MauMedia {
    /// The canonical IEEE designation, e.g. `1000BASE-T`.
    pub media: &'static str,
    /// The duplex the registration itself encodes, where it encodes one.
    ///
    /// A **secondary** source for duplex: `dot3StatsDuplexStatus` is read on the fast path and is
    /// the primary. This fills in only where that answered nothing, which on a device that
    /// implements MAU-MIB but not EtherLike-MIB is every port.
    pub duplex: Option<Duplex>,
}

/// Resolve an `ifMauType` OID value to a media type.
///
/// The value arrives as dotted decimal (`1.3.6.1.2.1.26.4.30`); the meaningful part is the final
/// sub-identifier under [`DOT3_MAU_TYPE_ROOT`]. Reading a sub-identifier out of an OID-valued column
/// is an established shape here — `optical::EntityIndex` does the same with
/// `entAliasMappingIdentifier`.
///
/// A value outside the arc, or a registration this table does not carry, is `None`. **Never a
/// guess**: the caller logs the sub-identifier at debug so a real gap is discoverable from a running
/// deployment rather than showing an operator the wrong medium.
#[must_use]
pub fn media_from_mau_oid(value: &str) -> Option<MauMedia> {
    let tail = value
        .trim()
        .strip_prefix(DOT3_MAU_TYPE_ROOT)?
        .strip_prefix('.')?;
    // Exactly one sub-identifier below the arc — `…26.4.30.1` is not a registration.
    let subid: u32 = tail.parse().ok()?;
    MAU_TYPES
        .iter()
        .find(|(id, _, _)| *id == subid)
        .map(|(_, media, duplex)| MauMedia {
            media,
            duplex: *duplex,
        })
}

/// Shortest designation this module will accept from free vendor text.
///
/// The registry's early entries are short enough to appear inside unrelated words — `AUI` (3) would
/// match a part number containing those letters, `10BASET` (7) sits inside `100BASET4`. Nothing that
/// ships in a pluggable reports one of them anyway, so the floor costs nothing real and removes the
/// whole class of accident.
const MIN_TEXT_MATCH_LEN: usize = 8;

/// Uppercase and drop everything that is not a letter or digit.
///
/// `SFP-1000BaseLX` and `1000BASE-LX` both become `…1000BASELX`, which is what lets a vendor part
/// string be compared to a registration at all — hyphenation and case are exactly what vendors vary.
fn normalize_designation(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Find a canonical media designation inside a vendor's free-text transceiver string.
///
/// For the ENTITY-MIB fallback: `entPhysicalModelName` answers with a part number
/// (`SFP-1000BaseLX`, `OMXD30000`, `Finisar FTLX8571D3BCL`), which is **not** a media type. This
/// recovers one only when the string genuinely contains a designation, and returns `None` for
/// everything else — the part string then stays in `transceiver_model` alone, still useful, not
/// pretending to be something it is not.
///
/// **Deliberately strict, and expected to refuse most inputs.** Two rules do the work:
/// - the match is taken **longest-first**, so `1000BASE-LX10` wins over the `1000BASE-LX` inside it;
/// - designations shorter than [`MIN_TEXT_MATCH_LEN`] are never matched from text.
///
/// This is the same shape as `optical::reading_from_text`: refuse when ambiguous, because a wrong
/// medium shown confidently is worse than an empty cell.
#[must_use]
pub fn media_from_transceiver_text(text: &str) -> Option<&'static str> {
    let haystack = normalize_designation(text);
    if haystack.is_empty() {
        return None;
    }
    MAU_TYPES
        .iter()
        .map(|(_, media, _)| *media)
        .filter_map(|media| {
            let needle = normalize_designation(media);
            (needle.len() >= MIN_TEXT_MATCH_LEN && haystack.contains(&needle))
                .then_some((needle.len(), media))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, media)| media)
}

/// The final sub-identifier of an `ifMauType` value, for the debug log when it is unrecognised.
///
/// Separate from [`media_from_mau_oid`] so an unknown registration can be reported as a *number*
/// ("this device answered 103, which we do not carry") rather than as a whole OID string — that
/// number is what someone extending [`MAU_TYPES`] needs.
#[must_use]
pub fn mau_subid(value: &str) -> Option<u32> {
    value
        .trim()
        .strip_prefix(DOT3_MAU_TYPE_ROOT)?
        .strip_prefix('.')?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_round_trips_through_its_token_and_through_serde() {
        // Both directions, and the two mechanisms against each other: `as_str` is what the DB
        // column holds and `serde` is what the JSON field holds, and nothing but this test makes
        // them agree.
        for d in Duplex::ALL {
            assert_eq!(
                Duplex::parse(d.as_str()),
                Some(d),
                "token round-trip: {d:?}"
            );

            let json = serde_json::to_string(&d).expect("serialize");
            assert_eq!(
                json,
                format!("\"{}\"", d.as_str()),
                "serde tag must equal as_str for {d:?}"
            );
            let back: Duplex = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, d);
        }
    }

    #[test]
    fn an_unknown_duplex_token_is_none_not_a_default() {
        for token in ["", "auto", "Full", "fullDuplex", "3"] {
            assert_eq!(Duplex::parse(token), None, "token {token:?}");
        }
    }

    #[test]
    fn dot3_readings_map_to_the_two_real_modes() {
        // The accepting cases are the load-bearing half. A function that returned None for
        // everything would satisfy every rejection test below and ship a column that is always
        // empty — which is exactly what this feature would look like if it were broken.
        assert_eq!(duplex_from_dot3(2.0), Some(Duplex::Half));
        assert_eq!(duplex_from_dot3(3.0), Some(Duplex::Full));
    }

    #[test]
    fn unknown_and_out_of_range_dot3_readings_are_none() {
        // `unknown(1)` is a real and common answer, especially on optical ports where IEEE 802.3
        // defines no half duplex to negotiate. It stores as NULL, same as "never read".
        assert_eq!(duplex_from_dot3(1.0), None);
        for v in [0.0, 4.0, -1.0, 2.5, f64::NAN, f64::INFINITY] {
            assert_eq!(duplex_from_dot3(v), None, "value {v}");
        }
    }

    #[test]
    fn huawei_readings_map_to_the_two_real_modes() {
        // Accepting cases first, for the reason stated above: a mapper that returned None for
        // everything would satisfy every rejection test and leave the column exactly as blank as
        // it is today — the failure would be indistinguishable from "the feature never shipped".
        // 1 is what the lab USG returns on all twelve physical ports.
        assert_eq!(duplex_from_huawei(1.0), Some(Duplex::Full));
        assert_eq!(duplex_from_huawei(2.0), Some(Duplex::Half));
    }

    /// 🚨 The two enumerations disagree, and reusing one mapper for both would be silent.
    ///
    /// EtherLike `unknown(1)/half(2)/full(3)` vs Huawei `full(1)/half(2)`. They agree only on 2.
    /// A copy-paste of `duplex_from_dot3` into the Huawei path would read `full(1)` as `unknown`
    /// and store NULL for every port — no wrong value to notice, just nothing, which is precisely
    /// the symptom the Huawei path exists to remove.
    #[test]
    fn every_mapper_disagrees_where_the_enumerations_disagree() {
        assert_eq!(duplex_from_dot3(1.0), None);
        assert_eq!(duplex_from_huawei(1.0), Some(Duplex::Full));
        assert_eq!(duplex_from_dot3(3.0), Some(Duplex::Full));
        assert_eq!(duplex_from_huawei(3.0), None);
        // The one value they agree on, stated so a future edit that "unifies" them has to break
        // the two assertions above rather than this one.
        assert_eq!(duplex_from_dot3(2.0), duplex_from_huawei(2.0));
    }

    #[test]
    fn out_of_range_huawei_readings_are_none() {
        // No `unknown` in this enumeration, so 0 and 3 are both simply not values.
        for v in [0.0, 3.0, 4.0, -1.0, 1.5, f64::NAN, f64::INFINITY] {
            assert_eq!(duplex_from_huawei(v), None, "value {v}");
        }
    }

    /// The two duplex OIDs must be distinct subtrees, or the poller's `oid_base` demux would fold
    /// one column's rows into the other's map and read them with the wrong enumeration.
    #[test]
    fn the_two_duplex_columns_are_different_oids() {
        assert_ne!(OID_DOT3_DUPLEX_STATUS, OID_HW_ETHERNET_DUPLEX);
        assert!(!OID_HW_ETHERNET_DUPLEX.starts_with(OID_DOT3_DUPLEX_STATUS));
        assert!(!OID_DOT3_DUPLEX_STATUS.starts_with(OID_HW_ETHERNET_DUPLEX));
    }
    #[test]
    fn huawei_port_type_readings_map_to_the_two_real_media() {
        // Accepting cases first — the same reason as every other mapper here: a function that
        // returned None for everything would satisfy each rejection test below and leave the media
        // column exactly as blank as it is today, which is indistinguishable from "never shipped".
        assert_eq!(medium_from_huawei(2.0), Some(Medium::Copper));
        assert_eq!(medium_from_huawei(3.0), Some(Medium::Fiber));
    }

    #[test]
    fn other_and_out_of_range_port_types_are_none() {
        // `other(1)` is the agent saying it cannot state the type. That is "not known", not a third
        // medium, and must not become one.
        assert_eq!(medium_from_huawei(1.0), None);
        for v in [0.0, 4.0, -1.0, 2.5, f64::NAN, f64::INFINITY] {
            assert_eq!(medium_from_huawei(v), None, "value {v}");
        }
    }

    /// 🚨 The two Huawei columns share a table and must not share a mapper.
    ///
    /// `hwEthernetDuplex` is `full(1)/half(2)`; `hwEthernetPortType` is
    /// `other(1)/copper(2)/fiber(3)`. They overlap on every value and agree on none of them, so a
    /// copy-paste between the two would silently report every copper port as half duplex, or every
    /// full-duplex port as copper. Both are wrong *values*, which is worse than the empty cells this
    /// increment removes.
    #[test]
    fn the_duplex_and_port_type_enumerations_are_not_interchangeable() {
        assert_eq!(duplex_from_huawei(1.0), Some(Duplex::Full));
        assert_eq!(medium_from_huawei(1.0), None);
        assert_eq!(duplex_from_huawei(2.0), Some(Duplex::Half));
        assert_eq!(medium_from_huawei(2.0), Some(Medium::Copper));
        assert_eq!(duplex_from_huawei(3.0), None);
        assert_eq!(medium_from_huawei(3.0), Some(Medium::Fiber));
    }

    #[test]
    fn the_three_huawei_columns_are_distinct_oids() {
        // The poller demuxes this walk by `oid_base`. Two columns sharing a prefix would fold one
        // map into the other and read it with the wrong enumeration.
        let oids = [
            OID_HW_ETHERNET_DUPLEX,
            OID_HW_ETHERNET_PORT_TYPE,
            OID_DOT3_DUPLEX_STATUS,
        ];
        for (i, a) in oids.iter().enumerate() {
            for b in oids.iter().skip(i + 1) {
                assert_ne!(a, b);
                assert!(!a.starts_with(b), "{a} must not sit under {b}");
                assert!(!b.starts_with(a), "{b} must not sit under {a}");
            }
        }
    }

    #[test]
    fn a_copper_port_gets_the_designation_its_speed_defines() {
        // The four twisted-pair standards this feature can name. These are the speeds the lab's real
        // device actually reports: its two up ports run at 100 Mbit/s and 1 Gbit/s.
        assert_eq!(copper_designation(10_000_000), Some("10BASE-T"));
        assert_eq!(copper_designation(100_000_000), Some("100BASE-TX"));
        assert_eq!(copper_designation(1_000_000_000), Some("1000BASE-T"));
        assert_eq!(copper_designation(10_000_000_000), Some("10GBASE-T"));
    }

    /// The promise this function makes to its caller, asserted rather than described.
    ///
    /// Everything [`copper_designation`] emits must be a registration [`MAU_TYPES`] carries.
    /// That is what makes the derivation a *definition* rather than a guess, and what guarantees it
    /// can never disagree with the MAU-MIB walk that writes the same column.
    #[test]
    fn every_derived_designation_is_a_real_registration() {
        let mut seen = 0;
        for bps in [10_000_000i64, 100_000_000, 1_000_000_000, 10_000_000_000] {
            let d = copper_designation(bps).unwrap_or_else(|| panic!("{bps} bps must resolve"));
            assert!(
                MAU_TYPES.iter().any(|(_, media, _)| *media == d),
                "{d} is not a transcribed dot3MauType registration",
            );
            seen += 1;
        }
        assert_eq!(seen, 4, "all four derivable speeds were checked");
    }

    /// ⚠️ A speed with no *checked* twisted-pair registration answers nothing.
    ///
    /// 2.5G and 5G BASE-T are real; they sit above [`MAU_TYPES`]'s transcribed block, and that
    /// table's rule is to grow from the registry rather than from memory. Anything at or above 25G is
    /// twinax rather than twisted pair. Both cases are an empty cell, which is the honest answer.
    #[test]
    fn a_speed_with_no_transcribed_twisted_pair_standard_is_none() {
        for bps in [
            0i64,
            -1,
            64_000,
            2_500_000_000,
            5_000_000_000,
            25_000_000_000,
            40_000_000_000,
            100_000_000_000,
        ] {
            assert_eq!(copper_designation(bps), None, "{bps} bps");
        }
    }

    #[test]
    fn if_type_rejects_implausible_readings_and_keeps_real_ones() {
        assert_eq!(if_type_from_snmp(6.0), Some(IF_TYPE_ETHERNET_CSMACD));
        // tunnel(131) — a code this crate deliberately does not name, and must still carry through
        // untouched. The point of storing the raw integer is that a consumer can ask about codes
        // the backend has no opinion on.
        assert_eq!(if_type_from_snmp(131.0), Some(131));
        for v in [0.0, -3.0, 70000.0, 6.4, f64::NAN] {
            assert_eq!(if_type_from_snmp(v), None, "value {v}");
        }
    }

    /// The four designations this feature was asked for, by sub-identifier.
    ///
    /// ⚠️ **The accepting cases are the load-bearing half of this module's tests.** Every path here
    /// fails into `None`, which renders as an em dash — indistinguishable from a device that does
    /// not implement MAU-MIB. A lookup that returned `None` for everything would satisfy every
    /// rejection test below and ship a column that is empty forever, on every vendor.
    #[test]
    fn the_media_this_feature_was_built_for_map_correctly() {
        let cases = [
            (30, "1000BASE-T", Some(Duplex::Full)),
            (26, "1000BASE-SX", Some(Duplex::Full)),
            (24, "1000BASE-LX", Some(Duplex::Full)),
            (36, "10GBASE-SR", None),
        ];
        for (subid, media, duplex) in cases {
            let got = media_from_mau_oid(&format!("{DOT3_MAU_TYPE_ROOT}.{subid}"))
                .unwrap_or_else(|| panic!("sub-id {subid} must resolve"));
            assert_eq!(got.media, media, "sub-id {subid}");
            assert_eq!(got.duplex, duplex, "sub-id {subid} duplex");
        }
    }

    #[test]
    fn a_half_and_full_pair_share_one_media_name() {
        // This is what makes splitting media and duplex into two columns correct rather than
        // decorative: the registry encodes both in one value, and we take them apart.
        let hd = media_from_mau_oid("1.3.6.1.2.1.26.4.29").expect("1000BaseTHD");
        let fd = media_from_mau_oid("1.3.6.1.2.1.26.4.30").expect("1000BaseTFD");
        assert_eq!(hd.media, fd.media);
        assert_eq!(hd.duplex, Some(Duplex::Half));
        assert_eq!(fd.duplex, Some(Duplex::Full));
    }

    #[test]
    fn registrations_above_1g_carry_no_duplex() {
        // A pure transcription, not an inference. 802.3 defines no half duplex above 1 Gbit/s, so
        // "Full" would be *correct* here — and is deliberately not encoded, because a table with an
        // inference mixed in is not one a reviewer can check against the RFC line by line.
        for subid in [31, 36, 54, 72, 77] {
            let got = media_from_mau_oid(&format!("{DOT3_MAU_TYPE_ROOT}.{subid}")).unwrap();
            assert_eq!(got.duplex, None, "sub-id {subid}");
        }
    }

    #[test]
    fn an_unknown_or_malformed_mau_value_is_none_not_a_guess() {
        for v in [
            "1.3.6.1.2.1.26.4.79",   // past the transcribed block — the discoverable gap
            "1.3.6.1.2.1.26.4.999",  // no such registration
            "1.3.6.1.2.1.26.4.30.1", // two sub-ids below the arc is not a registration
            "1.3.6.1.2.1.26.4",      // the arc itself
            "1.3.6.1.2.1.2.2.1.3",   // a different OID entirely
            "",
            "not-an-oid",
        ] {
            assert!(media_from_mau_oid(v).is_none(), "value {v:?}");
        }
    }

    #[test]
    fn the_table_is_sorted_dense_and_free_of_duplicates() {
        // Makes a mis-paste loud. Dense-from-1 is what lets the cut be stated as a single number
        // ("transcribed through 78") rather than as a set of holes nobody can audit.
        for (i, (subid, media, _)) in MAU_TYPES.iter().enumerate() {
            assert_eq!(
                *subid,
                i as u32 + 1,
                "MAU_TYPES must be sorted and dense from 1; entry {i} is {subid}"
            );
            assert!(!media.is_empty(), "sub-id {subid} has no designation");
        }
        assert_eq!(MAU_TYPES.len(), 78, "the transcribed block ends at 78");
    }

    #[test]
    fn a_vendor_part_string_yields_its_designation_when_it_really_contains_one() {
        // The accepting half again. These are real-shaped `entPhysicalModelName` values.
        assert_eq!(
            media_from_transceiver_text("SFP-1000BaseLX"),
            Some("1000BASE-LX")
        );
        assert_eq!(
            media_from_transceiver_text("1000Base-SX SFP transceiver"),
            Some("1000BASE-SX")
        );
        assert_eq!(
            media_from_transceiver_text("10GBASE-SR"),
            Some("10GBASE-SR")
        );
    }

    #[test]
    fn the_longest_designation_wins_so_lx10_is_not_read_as_lx() {
        // `1000BASE-LX10` contains `1000BASE-LX`. Taking the longest is what stops a 10 km EFM optic
        // being reported as plain LX — different reach, different part, and an operator chasing a
        // link-budget problem would be misled by the shorter answer.
        assert_eq!(
            media_from_transceiver_text("SFP-1000BaseLX10"),
            Some("1000BASE-LX10")
        );
    }

    #[test]
    fn a_part_number_with_no_designation_in_it_is_refused() {
        // Refusing is the correct outcome, not a gap: these strings still reach the operator through
        // `transceiver_model`. Guessing a medium from them is what this function exists to avoid.
        for text in [
            "OMXD30000",
            "Finisar FTLX8571D3BCL",
            "QSFP-40G-SR4", // no "BASE" — close, and still not a designation
            "",
            "   ",
            "AUI",     // below the length floor
            "10BaseT", // likewise
        ] {
            assert_eq!(media_from_transceiver_text(text), None, "text {text:?}");
        }
    }

    #[test]
    fn mau_subid_extracts_the_number_for_the_gap_log() {
        assert_eq!(mau_subid("1.3.6.1.2.1.26.4.103"), Some(103));
        assert_eq!(mau_subid("1.3.6.1.2.1.26.4.30"), Some(30));
        assert_eq!(mau_subid("1.3.6.1.2.1.2.2.1.3"), None);
    }
}
