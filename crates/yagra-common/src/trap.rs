//! Well-known SNMP trap identity OIDs → human names.
//!
//! A small, curated static table mapping the standard RFC trap OIDs (plus the two BGP
//! FSM-transition traps) to their MIB names, so a received trap reads as `linkDown`
//! instead of `1.3.6.1.6.3.1.1.5.3` and operators can author event rules by name. This is
//! **not** a full MIB compiler — it covers the generic set every agent emits (RFC 3418
//! `snmpTraps`, which v1 generic traps map into per RFC 3584 §3.1) and the BGP4-MIB
//! transitions (RFC 4273). Anything outside the set resolves to `None`, and the caller
//! falls back to the raw dotted OID (which is always present in the event message, so a
//! rule can still match unknown traps by their numeric OID).
//!
//! Resolution is display/rule-matching sugar only — it is derived at read time from the
//! stored `trap_oid`, so extending this table also re-labels historical events.

/// Resolve a trap identity OID (dotted decimal, e.g. `"1.3.6.1.6.3.1.1.5.3"`) to its
/// well-known MIB name (e.g. `"linkDown"`), or `None` if it is not in the curated set.
#[must_use]
pub fn trap_oid_name(oid: &str) -> Option<&'static str> {
    Some(match oid {
        // Generic traps — RFC 3418 (SNMPv2-MIB) `snmpTraps` subtree. SNMPv1 generic traps
        // 0..5 map here per RFC 3584 §3.1 as `1.3.6.1.6.3.1.1.5.<generic + 1>`.
        "1.3.6.1.6.3.1.1.5.1" => "coldStart",
        "1.3.6.1.6.3.1.1.5.2" => "warmStart",
        "1.3.6.1.6.3.1.1.5.3" => "linkDown",
        "1.3.6.1.6.3.1.1.5.4" => "linkUp",
        "1.3.6.1.6.3.1.1.5.5" => "authenticationFailure",
        "1.3.6.1.6.3.1.1.5.6" => "egpNeighborLoss",
        // BGP4-MIB finite-state-machine transitions — RFC 4273.
        "1.3.6.1.2.1.15.7.1" => "bgpEstablished",
        "1.3.6.1.2.1.15.7.2" => "bgpBackwardTransition",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_standard_generic_traps() {
        assert_eq!(trap_oid_name("1.3.6.1.6.3.1.1.5.1"), Some("coldStart"));
        assert_eq!(trap_oid_name("1.3.6.1.6.3.1.1.5.3"), Some("linkDown"));
        assert_eq!(trap_oid_name("1.3.6.1.6.3.1.1.5.4"), Some("linkUp"));
        assert_eq!(
            trap_oid_name("1.3.6.1.6.3.1.1.5.5"),
            Some("authenticationFailure")
        );
    }

    #[test]
    fn resolves_the_bgp_transitions() {
        assert_eq!(trap_oid_name("1.3.6.1.2.1.15.7.1"), Some("bgpEstablished"));
        assert_eq!(
            trap_oid_name("1.3.6.1.2.1.15.7.2"),
            Some("bgpBackwardTransition")
        );
    }

    #[test]
    fn unknown_and_malformed_oids_are_none() {
        // A vendor-specific enterprise trap — not in the curated set.
        assert_eq!(trap_oid_name("1.3.6.1.4.1.9.9.43.2.0.1"), None);
        // Empty / non-OID input never panics, just misses.
        assert_eq!(trap_oid_name(""), None);
        assert_eq!(trap_oid_name("linkDown"), None);
        // A prefix of a known OID must not match (exact-string table, not prefix).
        assert_eq!(trap_oid_name("1.3.6.1.6.3.1.1.5"), None);
    }
}
