// SPDX-License-Identifier: AGPL-3.0-only
//! Dotted OID text: "is this under that", "what follows it", and the two trap varbinds every
//! trap carries (ADR-184).
//!
//! The rule that matters is the **dot boundary**: `…2.2.1.2` is not under `…2.2.1.20`, and a
//! reader that compares with a bare `starts_with` says it is. Five places wrote the boundary out by
//! hand — the SNMPv3 walker, the IP-MIB prefix pointer, both MAU readers and the interface-table
//! check — and the two trap constants were declared in the trap parser and again in the trap
//! renderer. They are said once here.
//!
//! What is deliberately **not** here: turning text into `snmp2`'s encoded OID (the trap renderer
//! needs 64-bit arcs and its own minimum length), and validating an operator-supplied OID at the
//! API edge (`api::util::is_valid_oid`, which answers a different question — "is this well formed",
//! not "where does it sit").

/// `snmpTrapOID.0` — the varbind a v2c trap names itself with (RFC 3416 §4.2.6).
pub const SNMP_TRAP_OID_0: &str = "1.3.6.1.6.3.1.1.4.1.0";

/// `sysUpTime.0` — the first varbind of every v2c trap, and a scalar the poller reads.
pub const SYS_UPTIME_0: &str = "1.3.6.1.2.1.1.3.0";

/// The text after `base.` when `oid` is a strict descendant of `base`; `None` for another subtree
/// or for `base` itself (a column with no instance).
#[must_use]
pub fn tail_under<'a>(oid: &'a str, base: &str) -> Option<&'a str> {
    oid.strip_prefix(base)?
        .strip_prefix('.')
        .filter(|t| !t.is_empty())
}

/// The sub-identifiers after `base.`, when `oid` is a strict descendant of `base` and every one of
/// them is a number.
#[must_use]
pub fn tail_subids(oid: &str, base: &str) -> Option<Vec<u32>> {
    tail_under(oid, base)?
        .split('.')
        .map(|p| p.parse::<u32>().ok())
        .collect()
}

/// Whether `oid` is `root` itself or anything beneath it.
#[must_use]
pub fn is_at_or_under(oid: &str, root: &str) -> bool {
    oid.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "1.3.6.1.2.1.2.2.1.2";

    #[test]
    fn a_descendant_is_one_that_continues_past_a_dot() {
        assert_eq!(tail_under("1.3.6.1.2.1.2.2.1.2.7", BASE), Some("7"));
        assert_eq!(
            tail_under("1.3.6.1.2.1.2.2.1.20.7", BASE),
            None,
            "…20 is not under …2"
        );
        assert_eq!(
            tail_under(BASE, BASE),
            None,
            "the column itself has no instance"
        );
        assert_eq!(tail_under("1.3.6.1.2.1.2.2.1.2.", BASE), None);
    }

    #[test]
    fn the_sub_identifiers_are_numbers_or_nothing() {
        assert_eq!(
            tail_subids("1.3.6.1.2.1.2.2.1.2.7.1", BASE),
            Some(vec![7, 1])
        );
        assert_eq!(tail_subids("1.3.6.1.2.1.2.2.1.2.7.x", BASE), None);
        assert_eq!(tail_subids("1.3.6.1.2.1.2.2.1.2.7..1", BASE), None);
    }

    #[test]
    fn a_root_contains_itself_and_its_subtree_and_nothing_that_only_shares_digits() {
        assert!(is_at_or_under(BASE, BASE));
        assert!(is_at_or_under("1.3.6.1.2.1.2.2.1.2.7", BASE));
        assert!(!is_at_or_under("1.3.6.1.2.1.2.2.1.20", BASE));
    }
}
