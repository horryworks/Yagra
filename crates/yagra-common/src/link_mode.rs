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
}
