// SPDX-License-Identifier: AGPL-3.0-only
//! Reserved primary-key ranges for the built-in catalog, in one place.
//!
//! Built-in profiles, collection templates, classification rules and the two seeded threshold sets
//! are inserted with **stable, derived ids** (`Uuid::from_u128(BASE + array_index)`) so that a
//! re-seed on every boot cannot clobber operator edits and cannot re-key an existing
//! node→profile binding. That scheme has a well-known hazard — inserting an entry mid-array
//! silently re-keys everything after it (`extensibility.md` §6) — and a second, quieter one: the
//! reserved ranges were written down in three unrelated places.
//!
//! Before this module the bases lived as five `const` declarations inside
//! `repo::seed_builtin_profiles`, as hexadecimal literals in migration `0020`'s range-DELETEs, and
//! as prose in that migration's header. The config bundle (ADR-040 decision 3) needed a fourth
//! copy, to answer "is this row a built-in?" when deciding what to export — and a filter that
//! disagreed with the seeder would either export rows that re-key on import, or silently drop
//! operator-created rows that happened to look built-in.
//!
//! So the seeder and the filter now read the same table, and two tests pin it to the migrations
//! that also state it. There is nothing to keep in sync by hand.

use uuid::Uuid;

/// How many ids each range reserves. Matches the `…00` … `…ff` spans migration `0020` deletes;
/// changing it would orphan rows that migration can no longer reach.
pub const RANGE_CAPACITY: u128 = 0x100;

/// One reserved block of stable seed ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedRange {
    /// `profiles` — [`yagra_common::builtin_profiles`].
    Profiles,
    /// `collection_templates` — [`yagra_common::builtin_templates`].
    CollectionTemplates,
    /// `classification_rules` — [`yagra_common::builtin_classification_rules`].
    ClassificationRules,
    /// `thresholds` seeded for the built-in URL / HTTP endpoint profile.
    UrlThresholds,
    /// `thresholds` seeded for the built-in DNS name-resolution profile.
    DnsThresholds,
    /// `thresholds` seeded at **global** scope — the fleet defaults every deployment starts
    /// with (ADR-075): reachability, SNMP agent health, packet loss.
    ///
    /// Unlike the two above these are not tied to a profile, so they reach a node with no
    /// profile at all — which is the reason the global scope exists.
    DefaultThresholds,
}

impl SeedRange {
    /// Every declared range. Iterated by [`is_builtin`] and by the disjointness test.
    pub const ALL: [SeedRange; 6] = [
        SeedRange::Profiles,
        SeedRange::CollectionTemplates,
        SeedRange::ClassificationRules,
        SeedRange::UrlThresholds,
        SeedRange::DnsThresholds,
        SeedRange::DefaultThresholds,
    ];

    /// The first id of the range.
    #[must_use]
    pub const fn base(self) -> u128 {
        match self {
            SeedRange::Profiles => 0x0000_0000_0000_0000_0000_0000_5eed_0000,
            SeedRange::CollectionTemplates => 0x0000_0000_0000_0000_0000_0000_5eed_7000,
            SeedRange::ClassificationRules => 0x0000_0000_0000_0000_0000_0000_5eed_8000,
            SeedRange::UrlThresholds => 0x0000_0000_0000_0000_0000_0000_5eed_a000,
            SeedRange::DnsThresholds => 0x0000_0000_0000_0000_0000_0000_5eed_b000,
            SeedRange::DefaultThresholds => 0x0000_0000_0000_0000_0000_0000_5eed_c000,
        }
    }

    /// The stable id for the `index`-th built-in of this range.
    ///
    /// `index` is the **array position** in the corresponding `yagra_common::builtin_*()` list, so
    /// inserting an entry mid-array re-keys every later row. Always append.
    #[must_use]
    pub fn id(self, index: usize) -> Uuid {
        Uuid::from_u128(self.base() + index as u128)
    }

    /// Whether `id` falls inside this reserved range.
    #[must_use]
    pub fn contains(self, id: Uuid) -> bool {
        let n = id.as_u128();
        n >= self.base() && n < self.base() + RANGE_CAPACITY
    }
}

/// The built-in SNMP-trap event rules seeded by migration `0047`.
///
/// Fixed ids rather than a derived range: they are seeded by a migration, once, and are ordinary
/// operator-owned rows afterwards (editable, disable-able, deletable). They are listed here for the
/// one reason the ranges are — so the bundle exporter can leave them out of a bundle that would
/// otherwise recreate a rule the operator deliberately deleted on the target.
pub const BUILTIN_EVENT_RULE_IDS: [Uuid; 5] = [
    Uuid::from_u128(0xb000_0001_0000_4000_8000_0000_0000_0001),
    Uuid::from_u128(0xb000_0002_0000_4000_8000_0000_0000_0002),
    Uuid::from_u128(0xb000_0003_0000_4000_8000_0000_0000_0003),
    Uuid::from_u128(0xb000_0004_0000_4000_8000_0000_0000_0004),
    Uuid::from_u128(0xb000_0005_0000_4000_8000_0000_0000_0005),
];

/// Whether `id` is a built-in seed row — inside a reserved range or a seeded event rule.
///
/// The config bundle uses this as its export filter. A built-in already exists on the target (it is
/// seeded there too), and carrying one across would at best be a no-op and at worst re-key a row
/// whose index differs between the two builds' catalogs.
#[must_use]
pub fn is_builtin(id: Uuid) -> bool {
    SeedRange::ALL.iter().any(|r| r.contains(id)) || BUILTIN_EVENT_RULE_IDS.contains(&id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ranges are 0x100 apart at their closest; an overlap would make one catalog's re-seed
    /// silently shadow another's rows.
    #[test]
    fn every_declared_range_is_disjoint() {
        for (i, a) in SeedRange::ALL.iter().enumerate() {
            for b in &SeedRange::ALL[i + 1..] {
                assert!(
                    a.base() + RANGE_CAPACITY <= b.base() || b.base() + RANGE_CAPACITY <= a.base(),
                    "{a:?} and {b:?} overlap"
                );
            }
        }
    }

    /// Every row the seeder actually writes must land inside the range the range-DELETE migration
    /// can reach. The 257th built-in profile would otherwise be seeded outside it, be invisible to
    /// a future re-seed, and start being exported in config bundles as if an operator had made it.
    #[test]
    fn every_seeded_row_falls_inside_its_declared_range() {
        let catalogs: [(SeedRange, usize); 3] = [
            (SeedRange::Profiles, yagra_common::builtin_profiles().len()),
            (
                SeedRange::CollectionTemplates,
                yagra_common::builtin_templates().len(),
            ),
            (
                SeedRange::ClassificationRules,
                yagra_common::builtin_classification_rules().len(),
            ),
        ];
        for (range, count) in catalogs {
            assert!(count > 0, "{range:?} catalog is empty");
            for i in 0..count {
                assert!(
                    range.contains(range.id(i)),
                    "{range:?}[{i}] is outside its reserved range"
                );
                assert!(is_builtin(range.id(i)));
            }
        }
    }

    /// Headroom, stated as a number rather than left to be discovered when a catalog grows into the
    /// next range. `0x100` is 256 entries per catalog.
    #[test]
    fn each_range_has_headroom_for_the_current_catalog() {
        for (range, count) in [
            (SeedRange::Profiles, yagra_common::builtin_profiles().len()),
            (
                SeedRange::CollectionTemplates,
                yagra_common::builtin_templates().len(),
            ),
            (
                SeedRange::ClassificationRules,
                yagra_common::builtin_classification_rules().len(),
            ),
        ] {
            assert!(
                (count as u128) < RANGE_CAPACITY,
                "{range:?} has {count} entries and the range reserves {RANGE_CAPACITY}; \
                 growing past it needs a new range plus a migration that deletes it"
            );
        }
    }

    /// Migration `0020` deletes the built-in catalog by hexadecimal literal. This asserts the
    /// literals it uses are the bases and ends this module declares — the one pairing that, when it
    /// drifts, leaves stale seed rows shadowing the new catalog with no error anywhere.
    #[test]
    fn the_reserved_ranges_are_the_ones_the_range_delete_migration_names() {
        let sql = include_str!("../../../migrations/0020_reseed_builtin_catalog.sql");
        for range in [
            SeedRange::Profiles,
            SeedRange::CollectionTemplates,
            SeedRange::ClassificationRules,
        ] {
            let first = range.id(0).to_string();
            let last = Uuid::from_u128(range.base() + RANGE_CAPACITY - 1).to_string();
            assert!(
                sql.contains(&first),
                "{range:?}: 0020 does not name {first}"
            );
            assert!(sql.contains(&last), "{range:?}: 0020 does not name {last}");
        }
    }

    /// Migration `0091` names four profile ids and one template id as literals; this is what stops
    /// them from being a hand-written fifth copy of "seed id = array index".
    ///
    /// The ids in that file were verified against a running deployment when it was written, which
    /// proves they were right **then**. Appending a profile before one of these four in
    /// `builtin_profiles()` would silently re-point the literal at a different profile, and the
    /// migration would quietly delete the wrong link on every deployment that had not run it yet.
    /// `collection.rs::the_template_order_is_the_one_seed_ids_were_issued_for` guards the template
    /// array the same way; this guards the pairing the SQL depends on.
    #[test]
    fn migration_0091_names_the_ids_the_catalog_issues_today() {
        let sql = include_str!("../../../migrations/0091_cisco_optical_dialect.sql");
        let templates = yagra_common::builtin_templates();
        let std_optical = templates
            .iter()
            .position(|t| t.name == yagra_common::OpticalFlavor::EntitySensor.template_name())
            .expect("the standard optical template is still in the catalog");
        let template_id = SeedRange::CollectionTemplates.id(std_optical).to_string();
        assert!(
            sql.contains(&template_id),
            "0091 deletes links to {template_id} but the catalog now issues a different id"
        );

        let profiles = yagra_common::builtin_profiles();
        for name in [
            "Cisco IOS/IOS-XE router",
            "Cisco IOS-XR router",
            "Cisco Catalyst switch (IOS/IOS-XE)",
            "Cisco Nexus switch (NX-OS)",
        ] {
            let i = profiles
                .iter()
                .position(|p| p.name == name)
                .unwrap_or_else(|| panic!("built-in profile {name} disappeared"));
            let id = SeedRange::Profiles.id(i).to_string();
            assert!(sql.contains(&id), "0091 does not name {name} ({id})");
        }

        // The ASA keeps the standard dialect (its walk has 15 rows there and none in Cisco's), so
        // its id must NOT appear. Stated as an assertion because "we left one out" is exactly the
        // kind of intent that reads as an oversight to the next person.
        let asa = profiles
            .iter()
            .position(|p| p.name == "Cisco ASA firewall")
            .expect("the ASA profile is still in the catalog");
        assert!(
            !sql.contains(&SeedRange::Profiles.id(asa).to_string()),
            "0091 must leave the ASA on the standard dialect"
        );
    }

    /// Migration `0097` re-points the vendor default thresholds by deleting offsets 4..=23 of
    /// `DefaultThresholds` — as two hexadecimal literals, because SQL cannot call this enum.
    ///
    /// The pairing is the whole risk. A literal one digit off deletes the wrong rows: too low and
    /// it takes out node-down or SNMP-down (which the re-seed then restores, so nothing looks
    /// broken and the fleet simply stops paging until the next boot); too high and it leaves the
    /// old `global` rows in place, where `ON CONFLICT DO NOTHING` makes them permanent.
    #[test]
    fn the_reseeded_threshold_offsets_are_the_ones_migration_0097_deletes() {
        let sql = include_str!("../../../migrations/0097_reseed_vendor_thresholds.sql");
        let first = SeedRange::DefaultThresholds.id(4).to_string();
        let last = SeedRange::DefaultThresholds.id(23).to_string();
        assert!(sql.contains(&first), "0097 does not name {first}");
        assert!(sql.contains(&last), "0097 does not name {last}");
        // And the rows it must NOT take: the four fleet-wide defaults below the range, and the
        // first id above it. Naming them is what makes the bounds a range rather than a prefix.
        for keep in [0usize, 1, 2, 3, 24] {
            let id = SeedRange::DefaultThresholds.id(keep).to_string();
            assert!(
                !sql.contains(&id),
                "0097 must not name {id} (offset {keep})"
            );
        }
    }
    /// The same pairing for the seeded trap rules, which migration `0047` writes as literals.
    #[test]
    fn the_builtin_event_rule_ids_are_the_ones_migration_0047_inserts() {
        let sql = include_str!("../../../migrations/0047_builtin_trap_rules.sql");
        for id in BUILTIN_EVENT_RULE_IDS {
            assert!(sql.contains(&id.to_string()), "0047 does not insert {id}");
        }
    }

    /// An operator-created row (random v4) must never read as built-in, or the exporter would drop
    /// it from every bundle and the operator would find their work missing on the target.
    #[test]
    fn a_random_id_is_not_mistaken_for_a_builtin() {
        for _ in 0..64 {
            assert!(!is_builtin(Uuid::new_v4()));
        }
        // The id one past the end of a range is outside it — an off-by-one here would either
        // shadow an operator row or leak a seed row.
        assert!(!SeedRange::Profiles
            .contains(Uuid::from_u128(SeedRange::Profiles.base() + RANGE_CAPACITY)));
    }
}
