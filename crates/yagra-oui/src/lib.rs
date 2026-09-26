// SPDX-License-Identifier: AGPL-3.0-only
//! The maker behind a MAC address, from the IEEE Registration Authority's public listings
//! (ADR-180).
//!
//! The three registries assign prefixes of three lengths — MA-L 24 bits, MA-M 28, MA-S 36 — and a
//! longer one is carved out of a shorter block the IEEE holds itself, so a lookup takes the
//! **longest** prefix that matches.
//!
//! **Display only.** A MAC's prefix names who made the *network interface*, which on a whitebox
//! switch or a virtual machine is not whoever wrote the software answering SNMP. Nothing here may
//! pre-fill a node's vendor: classification reads `sysDescr` for that, and a wrong guess stored as
//! an operator-set value would outlive the guess (`api/discovery.rs` says the same).
//!
//! The table is `data/oui.tsv`, committed and compiled in — never fetched at build time, because
//! images are built offline-capable and deployments may sit on closed networks.
//! `scripts/oui-refresh.mjs` regenerates it; `/docs` runs it before a release.

use std::sync::OnceLock;

/// The committed registry: `#` header lines, then `PREFIX<TAB>organization`, prefix in 6, 7 or 9
/// upper-case hex digits.
const REGISTRY: &str = include_str!("../data/oui.tsv");

/// The prefix lengths the IEEE assigns, longest first — the order a lookup tries them in.
const PREFIX_BITS: [u32; 3] = [36, 28, 24];

/// The name the IEEE registers its own MA-L blocks under — the parents MA-M and MA-S assignments
/// are carved from. It never made anything, so a MAC whose longest match is one of these blocks has
/// no known maker (the sub-block is unassigned, or was withheld from the table), and the lookup says
/// so rather than naming the registry as a manufacturer.
const IEEE_PARENT: &str = "IEEE Registration Authority";

/// One table per prefix length, each sorted by prefix for a binary search.
struct Table {
    by_len: [Vec<(u64, &'static str)>; 3],
    fetched: &'static str,
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| parse(REGISTRY))
}

fn parse(text: &'static str) -> Table {
    let mut by_len: [Vec<(u64, &'static str)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut fetched = "";
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# fetched: ") {
            fetched = rest.trim();
            continue;
        }
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((prefix, org)) = line.split_once('\t') else {
            continue;
        };
        let Ok(value) = u64::from_str_radix(prefix, 16) else {
            continue;
        };
        let slot = match prefix.len() {
            9 => 0,
            7 => 1,
            6 => 2,
            _ => continue,
        };
        by_len[slot].push((value, org));
    }
    for t in &mut by_len {
        t.sort_unstable_by_key(|(p, _)| *p);
    }
    Table { by_len, fetched }
}

/// The organization the IEEE registered the MAC's prefix to, or `None` when no registry covers it.
///
/// Also `None` for a **locally administered** address (the second-lowest bit of the first octet —
/// the owner chose it, no maker did; virtual interfaces and randomized MACs look like this) and for
/// a **multicast** one (the lowest bit), which never names a single interface.
#[must_use]
pub fn vendor(mac: [u8; 6]) -> Option<&'static str> {
    if mac[0] & 0b11 != 0 {
        return None;
    }
    let value = mac.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
    let t = table();
    for (slot, bits) in PREFIX_BITS.iter().enumerate() {
        let prefix = value >> (48 - bits);
        let rows = &t.by_len[slot];
        if let Ok(i) = rows.binary_search_by_key(&prefix, |(p, _)| *p) {
            let org = rows[i].1;
            return (org != IEEE_PARENT).then_some(org);
        }
    }
    None
}

/// Parse a MAC written as six hex octets separated by `:` or `-`, in either case. `None` for
/// anything else — a text id that merely looks like a MAC is the caller's to rule out first.
#[must_use]
pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.trim().split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = [0u8; 6];
    for (slot, part) in out.iter_mut().zip(&parts) {
        if part.len() != 2 {
            return None;
        }
        *slot = u8::from_str_radix(part, 16).ok()?;
    }
    Some(out)
}

/// The date the committed registry was fetched (`YYYY-MM-DD`), for saying how old an answer is.
#[must_use]
pub fn registry_fetched() -> &'static str {
    table().fetched
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `# counts:` header, as `(registry, count)` pairs.
    fn header_counts() -> Vec<(String, usize)> {
        let line = REGISTRY
            .lines()
            .find_map(|l| l.strip_prefix("# counts: "))
            .expect("the header carries counts");
        line.split_whitespace()
            .filter_map(|kv| kv.split_once('='))
            .filter(|(k, _)| k.starts_with("MA-") && !k.contains("repeats"))
            .map(|(k, v)| (k.to_owned(), v.parse().expect("a count is a number")))
            .collect()
    }

    #[test]
    fn the_committed_registry_parses_sorted_and_unique() {
        let t = parse(REGISTRY);
        for rows in &t.by_len {
            assert!(
                rows.windows(2).all(|w| w[0].0 < w[1].0),
                "a prefix repeats or the table is unsorted"
            );
            assert!(rows.iter().all(|(_, org)| !org.trim().is_empty()));
        }
    }

    /// Catches a truncated download: the header's counts were taken from the full CSVs.
    #[test]
    fn the_header_counts_match_the_rows() {
        let t = parse(REGISTRY);
        let expected = header_counts();
        let by_name = [("MA-S", 0), ("MA-M", 1), ("MA-L", 2)];
        assert_eq!(expected.len(), 3, "the header names all three registries");
        for (name, count) in expected {
            let slot = by_name.iter().find(|(n, _)| *n == name).expect("known").1;
            assert_eq!(t.by_len[slot].len(), count, "{name}");
        }
    }

    /// A floor well under the real sizes — enough that a file cut to a few hundred rows fails.
    #[test]
    fn every_registry_meets_its_floor() {
        let t = parse(REGISTRY);
        assert!(t.by_len[2].len() >= 30_000, "MA-L");
        assert!(t.by_len[1].len() >= 4_000, "MA-M");
        assert!(t.by_len[0].len() >= 4_000, "MA-S");
    }

    #[test]
    fn well_known_prefixes_resolve() {
        let cisco = vendor([0x00, 0x00, 0x0c, 0x12, 0x34, 0x56]).unwrap();
        assert!(cisco.starts_with("Cisco"), "{cisco}");
        let vmware = vendor([0x00, 0x50, 0x56, 0xab, 0xcd, 0xef]).unwrap();
        assert!(vmware.starts_with("VMware"), "{vmware}");
    }

    /// A 36-bit assignment sits inside a 24-bit block the IEEE holds; the longer one must answer.
    /// Taken from the data rather than hard-coded, so a refresh cannot make this vacuous.
    #[test]
    fn a_longer_prefix_wins_over_the_block_it_is_carved_from() {
        let t = parse(REGISTRY);
        let (prefix36, org36) = t.by_len[0]
            .iter()
            .copied()
            .find(|(p, _)| {
                let parent = p >> 12;
                t.by_len[2]
                    .binary_search_by_key(&parent, |(q, _)| *q)
                    .is_ok()
            })
            .expect("some MA-S block sits under a listed MA-L");
        let value = prefix36 << 12;
        let mac: [u8; 6] = std::array::from_fn(|i| (value >> (40 - 8 * i)) as u8);
        assert_eq!(vendor(mac), Some(org36));
    }

    /// Every block the IEEE holds for itself is a parent of MA-M/MA-S assignments. A MAC that falls
    /// in one but in no assignment beneath it must name nobody — never the registry.
    #[test]
    fn the_registry_itself_is_never_named_as_a_maker() {
        let t = parse(REGISTRY);
        let parents: Vec<u64> = t.by_len[2]
            .iter()
            .filter(|(_, org)| *org == IEEE_PARENT)
            .map(|(p, _)| *p)
            .collect();
        assert!(!parents.is_empty(), "the data carries IEEE parent blocks");
        for p in parents {
            for low in [0u64, 0xff_ffff] {
                let value = (p << 24) | low;
                let mac: [u8; 6] = std::array::from_fn(|i| (value >> (40 - 8 * i)) as u8);
                assert_ne!(vendor(mac), Some(IEEE_PARENT), "{mac:02x?}");
            }
        }
    }

    #[test]
    fn locally_administered_and_multicast_addresses_name_no_maker() {
        // 00:00:0c is Cisco's; the same octets with the local bit set are nobody's.
        assert_eq!(vendor([0x02, 0x00, 0x0c, 0x12, 0x34, 0x56]), None);
        assert_eq!(vendor([0x01, 0x00, 0x5e, 0x00, 0x00, 0x01]), None);
    }

    #[test]
    fn parse_mac_accepts_both_separators_and_either_case_and_nothing_else() {
        let want = Some([0x00, 0x1b, 0x54, 0xff, 0x00, 0x9a]);
        assert_eq!(parse_mac("00:1b:54:ff:00:9a"), want);
        assert_eq!(parse_mac("00-1B-54-FF-00-9A"), want);
        assert_eq!(parse_mac("001b.54ff.009a"), None);
        assert_eq!(parse_mac("00:1b:54:ff:00"), None);
        assert_eq!(parse_mac("00:1b:54:ff:00:9g"), None);
        assert_eq!(parse_mac("0:1b:54:ff:00:9a"), None);
    }

    #[test]
    fn the_fetch_date_is_read_from_the_header() {
        let d = registry_fetched();
        assert_eq!(d.len(), 10, "{d}");
        assert!(d.starts_with("20"), "{d}");
    }
}
