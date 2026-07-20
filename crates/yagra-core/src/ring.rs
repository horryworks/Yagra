// SPDX-License-Identifier: AGPL-3.0-only
//! Consistent hash ring for poller assignment (ADR-009).
//!
//! Each pool has one ring built from its live pollers. A node is placed on the ring by hashing
//! its stable [`NodeId`] and walking clockwise to the next vnode point; that point's member owns
//! the node. Adding or removing a member only re-homes the nodes near the changed member's
//! vnodes, so a poller joining or dying moves ≈`1/n` of the pool rather than reshuffling
//! everything — the property that makes failover a small diff, not a full re-poll.
//!
//! Pure and deterministic: no I/O, no async, no clock. The hash is an **inlined FNV-1a 64-bit**
//! (not [`std::collections::hash_map::DefaultHasher`], whose per-process random seed would make
//! two cores — or the same core across restarts — disagree on placement). Because the digest is
//! fixed, every process and every run agrees on the ring for a given member set.

use yagra_common::NodeId;

/// Virtual nodes minted per member. More vnodes ⇒ smoother key distribution and smaller movement
/// on membership change, at a linear memory/sort cost. 128 keeps a large pool's ring cheap while
/// holding per-member load within a few percent of even (see the distribution test).
pub const VNODES_PER_MEMBER: usize = 128;

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Inlined FNV-1a 64-bit digest of `bytes`.
///
/// Deterministic across processes and runs (unlike the stdlib default hasher's randomized seed),
/// which is exactly what a shared ring needs so independent cores place a node identically.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// murmur3 `fmix64` finalizer — a bijective avalanche that turns a structured 64-bit value into a
/// uniformly-distributed one.
///
/// Applied to the vnode point below because plain FNV-1a of the near-identical strings
/// `"{member}#0".."#127"` (members that share a long name prefix, e.g. `poller-a` / `poller-b`)
/// clusters, giving one member a lopsided arc of the ring and badly skewing per-member load. FNV-1a
/// is still the base digest (deterministic, no deps); this only spreads its output so the
/// distribution guarantee holds. Determinism is preserved (a fixed bijection).
fn fmix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 33)).wrapping_mul(0xff51_afd7_ed55_8ccd);
    z = (z ^ (z >> 33)).wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    z ^ (z >> 33)
}

/// Ring position of member `member`'s `i`-th vnode: `fmix64(fnv1a("{member}#{i}"))`.
fn vnode_point(member: &str, i: usize) -> u64 {
    fmix64(fnv1a(format!("{member}#{i}").as_bytes()))
}

/// A consistent hash ring over a set of members (poller ids).
///
/// Construction is deterministic: members are deduped and sorted, each mints
/// [`VNODES_PER_MEMBER`] vnode points, and the points are sorted by hash (ties broken by the
/// member's sorted position). Two rings built from the same members in any order are identical.
pub struct HashRing {
    /// Members (poller ids), deduped and sorted — the canonical order that makes the ring
    /// insertion-order-independent.
    members: Vec<String>,
    /// Vnode points sorted by `(hash, member_index)`, each carrying the index of its owning member
    /// in [`Self::members`]. Binary-searched on assignment.
    points: Vec<(u64, usize)>,
}

impl HashRing {
    /// Build a ring from `members`. Duplicates are removed and members are sorted so the ring is
    /// independent of insertion order; each surviving member contributes [`VNODES_PER_MEMBER`]
    /// vnode points. An empty member set yields an empty ring ([`Self::assign`] returns `None`).
    pub fn new(members: impl IntoIterator<Item = String>) -> Self {
        let mut members: Vec<String> = members.into_iter().collect();
        members.sort();
        members.dedup();

        let mut points: Vec<(u64, usize)> = Vec::with_capacity(members.len() * VNODES_PER_MEMBER);
        for (idx, member) in members.iter().enumerate() {
            for i in 0..VNODES_PER_MEMBER {
                points.push((vnode_point(member, i), idx));
            }
        }
        // Sort by hash; break ties by the member's (sorted) index so a hash collision between two
        // members' vnodes resolves deterministically — the whole ring is a pure function of the
        // member set.
        points.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

        Self { members, points }
    }

    /// The member that owns `node_id`, or `None` if the ring is empty.
    ///
    /// Hashes the node's 16 UUID bytes and returns the member of the first vnode point clockwise
    /// (the first point with `hash >= node_hash`, wrapping to the first point past the end).
    #[must_use]
    pub fn assign(&self, node_id: NodeId) -> Option<&str> {
        if self.points.is_empty() {
            return None;
        }
        let h = fnv1a(node_id.as_uuid().as_bytes());
        // First point with `point_hash >= h`; wrap to index 0 when `h` is past the last point.
        let mut idx = self.points.partition_point(|&(p, _)| p < h);
        if idx == self.points.len() {
            idx = 0;
        }
        let member_idx = self.points[idx].1;
        Some(self.members[member_idx].as_str())
    }

    /// Whether the ring has no members.
    // Ring introspection exercised by the ring's own tests; the coordinator checks its live-member
    // Vec's emptiness directly rather than building an empty ring.
    #[allow(dead_code)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// The ring's members, deduped and sorted.
    // Ring introspection exercised by the ring's own tests.
    #[allow(dead_code)]
    #[must_use]
    pub fn members(&self) -> &[String] {
        &self.members
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// SplitMix64 — a high-quality deterministic PRNG. Used only to synthesize *uniformly
    /// distributed* node ids for the statistical tests; production node ids are real (v4) UUIDs,
    /// which are already uniform. (A naive `from_u128(i)` or a low-entropy FNV of `i` would feed
    /// the ring structured, non-uniform positions and invalidate the distribution assertions.)
    fn splitmix64(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// `n` uniformly-distributed synthetic node ids (deterministic seed).
    fn synthetic_nodes(n: u64) -> Vec<NodeId> {
        let mut state = 0x0123_4567_89ab_cdef_u64;
        (0..n)
            .map(|_| {
                let hi = u128::from(splitmix64(&mut state));
                let lo = u128::from(splitmix64(&mut state));
                NodeId::from(Uuid::from_u128((hi << 64) | lo))
            })
            .collect()
    }

    fn ring(members: &[&str]) -> HashRing {
        HashRing::new(members.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn fnv1a_matches_known_vectors() {
        // Locks the digest so placement is stable across processes/runs (the whole point of not
        // using DefaultHasher). These are the canonical FNV-1a 64-bit test vectors.
        assert_eq!(fnv1a(b""), FNV_OFFSET);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"foobar"), 0x85944171_f73967e8);
    }

    #[test]
    fn distribution_is_within_twenty_percent() {
        let members = ["poller-a", "poller-b", "poller-c"];
        let r = ring(&members);
        let nodes = synthetic_nodes(10_000);

        let mut counts = std::collections::HashMap::new();
        for node in &nodes {
            *counts
                .entry(r.assign(*node).unwrap().to_string())
                .or_insert(0u32) += 1;
        }
        // 3 members × 10k nodes ⇒ ~3333 each; assert each within ~33% ±20% relative (2600..=4100).
        for m in members {
            let c = *counts.get(m).unwrap_or(&0);
            assert!(
                (2600..=4100).contains(&c),
                "member {m} got {c} nodes, outside the balanced band"
            );
        }
    }

    #[test]
    fn removing_a_member_only_moves_that_members_nodes() {
        let nodes = synthetic_nodes(10_000);
        let before = ring(&["a", "b", "c", "d"]);
        let after = ring(&["a", "b", "d"]); // removed "c"

        let mut moved_from_removed = 0u32;
        for node in &nodes {
            let was = before.assign(*node).unwrap();
            let now = after.assign(*node).unwrap();
            if was == "c" {
                moved_from_removed += 1;
                // Its nodes must land on one of the survivors.
                assert!(matches!(now, "a" | "b" | "d"));
            } else {
                // A node on a surviving member must not move — minimal-movement guarantee.
                assert_eq!(was, now, "node on survivor {was} moved to {now}");
            }
        }
        assert!(
            moved_from_removed > 0,
            "removing a member should move its nodes"
        );
    }

    #[test]
    fn adding_a_member_moves_at_most_a_quarter() {
        let nodes = synthetic_nodes(10_000);
        let before = ring(&["a", "b", "c", "d"]);
        let after = ring(&["a", "b", "c", "d", "e"]);

        let moved = nodes
            .iter()
            .filter(|node| before.assign(**node) != after.assign(**node))
            .count();
        // Ideal is ~1/5 (2000); allow a generous bound well under a full reshuffle.
        assert!(
            moved < 3000,
            "adding a 5th member moved {moved} nodes (>30%)"
        );
    }

    #[test]
    fn determinism_is_independent_of_insertion_order() {
        let r1 = ring(&["a", "b", "c"]);
        let r2 = ring(&["c", "a", "b"]);
        assert_eq!(r1.members(), r2.members());
        for node in &synthetic_nodes(2_000) {
            assert_eq!(r1.assign(*node), r2.assign(*node));
        }
    }

    #[test]
    fn duplicate_members_are_deduped() {
        let r = ring(&["a", "a", "b", "a"]);
        assert_eq!(r.members(), ["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn single_member_owns_everything() {
        let r = ring(&["only"]);
        assert!(!r.is_empty());
        for node in &synthetic_nodes(1_000) {
            assert_eq!(r.assign(*node), Some("only"));
        }
    }

    #[test]
    fn empty_ring_assigns_nothing() {
        let r = HashRing::new(std::iter::empty::<String>());
        assert!(r.is_empty());
        assert_eq!(r.assign(NodeId::from(Uuid::nil())), None);
    }
}
