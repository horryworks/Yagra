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

import type { TFunction } from 'i18next';

/**
 * Every tier the backend defines, in cadence order (most frequent → least).
 *
 * Mirrors `MerakiTier::ALL` in `crates/yagra-common/src/meraki.rs`. Hand-maintained because the
 * tier is a bare string in the API (`MerakiOrgView.enabled_tiers`), so nothing generated carries
 * the set.
 */
export const MERAKI_TIERS = [
  'availability',
  'uplink',
  'wireless',
  'switch_ports',
  'traffic',
  'inventory',
] as const;

export type MerakiTier = (typeof MERAKI_TIERS)[number];

/**
 * The tiers the cadence dialog offers as checkboxes.
 *
 * A **subset**, deliberately: inventory is not a collect tier — core's `MerakiOrg::active_tiers()`
 * filters it out, and the inventory is read by the periodic sync on its own interval whether or not
 * the token is stored — so a checkbox for it would switch nothing. It still needs a label, because
 * the API accepts it and the org list prints what is stored. When `MerakiTier` grows, the test next
 * door fails and "is this one operator-selectable?" gets answered on purpose rather than by
 * omission.
 */
export const SELECTABLE_MERAKI_TIERS = [
  'availability',
  'uplink',
  'wireless',
  'switch_ports',
  'traffic',
] as const satisfies readonly MerakiTier[];

export type SelectableMerakiTier = (typeof SELECTABLE_MERAKI_TIERS)[number];

/**
 * The tier an organization cannot go without (ADR-164 決定 17).
 *
 * Availability is the only tier that says whether a device is up — uplink and traffic record
 * readings and decide nothing. An organization saved without it gave its nodes no liveness at all:
 * they sat in `unknown` and node-down could never fire. `PUT …/cadence` answers
 * `400 availability_required` for that shape, so the dialog does not offer it.
 */
export const REQUIRED_MERAKI_TIER = 'availability' satisfies SelectableMerakiTier;

/** The tiers the cadence dialog draws a checkbox for: the selectable ones that may be switched off.
 *  A checkbox that can only be refused is a control the operator cannot use, so the required tier
 *  gets a sentence instead (`meraki.cadence.availabilityAlways`). */
export const OPTIONAL_MERAKI_TIERS = SELECTABLE_MERAKI_TIERS.filter(
  (tier) => tier !== REQUIRED_MERAKI_TIER,
);

/** What a save sends as `enabled_tiers`: the ticked tiers, with the required one always in and
 *  first — where the column's own default has it. Anything already stored that the dialog does not
 *  draw (`inventory`) rides along untouched, as it did before. */
export function tiersToSave(ticked: Iterable<string>): string[] {
  const rest = [...new Set(ticked)].filter((tier) => tier !== REQUIRED_MERAKI_TIER);
  return [REQUIRED_MERAKI_TIER, ...rest];
}

/** The tiers a Meraki organization is collecting, as one localized list — or the "none" phrase.
 *  An empty list is a real state (an org added but not yet configured), so it gets a sentence
 *  rather than an empty cell. */
export const tierList = (tiers: string[], t: TFunction) =>
  tiers.length ? tiers.map((x) => t(`meraki.tier.${x}`)).join(', ') : t('meraki.tiersNone');
