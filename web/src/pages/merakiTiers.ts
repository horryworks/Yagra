// SPDX-License-Identifier: AGPL-3.0-only
// Cisco Meraki collection tiers, as data.
//
// The cadence dialog inlined `['availability', 'uplink', 'traffic'] as const` — three of the
// backend's four `MerakiTier` variants, with nothing recording that the fourth was left out on
// purpose. The org list meanwhile labels whatever the server stored, and the REST edge accepts
// every variant (`api/meraki.rs` validates against `MerakiTier::from_token`), so an org whose
// `enabled_tiers` carried `inventory` rendered the raw key `meraki.tier.inventory`.
//
// Kept in a `.ts` beside the page so the coverage tests can iterate it — Vitest only runs
// `src/**/*.test.ts`. `i18nEnumKeys.test.ts` demands both locales carry every tier's label, and the
// test next door pins the dialog's subset to the full list.

/**
 * Every tier the backend defines, in cadence order (most frequent → least).
 *
 * Mirrors `MerakiTier::ALL` in `crates/yagra-common/src/meraki.rs`. Hand-maintained because the
 * tier is a bare string in the API (`MerakiOrgView.enabled_tiers`), so nothing generated carries
 * the set.
 */
export const MERAKI_TIERS = ['availability', 'uplink', 'traffic', 'inventory'] as const;

export type MerakiTier = (typeof MERAKI_TIERS)[number];

/**
 * The tiers the cadence dialog offers as checkboxes.
 *
 * A **subset**, deliberately: inventory is reconciliation rather than a recurring collect — core's
 * `MerakiOrg::active_tiers()` filters it out and the operator triggers it from "Import devices" —
 * so a checkbox for it would promise polling that never happens. It still needs a label, because
 * the API accepts it and the org list prints what is stored. When `MerakiTier` grows, the test next
 * door fails and "is this one operator-selectable?" gets answered on purpose rather than by
 * omission.
 */
export const SELECTABLE_MERAKI_TIERS = [
  'availability',
  'uplink',
  'traffic',
] as const satisfies readonly MerakiTier[];

export type SelectableMerakiTier = (typeof SELECTABLE_MERAKI_TIERS)[number];
