// SPDX-License-Identifier: AGPL-3.0-only
// The cadence dialog's tier checkboxes, pinned to the backend's full tier list.

import { describe, expect, it } from 'vitest';
import {
  MERAKI_TIERS,
  OPTIONAL_MERAKI_TIERS,
  REQUIRED_MERAKI_TIER,
  SELECTABLE_MERAKI_TIERS,
  tierList,
  tiersToSave,
} from './merakiTiers';
import type { TFunction } from 'i18next';

describe('MERAKI_TIERS', () => {
  it('lists every backend tier exactly once, in cadence order', () => {
    // Mirrors `MerakiTier::ALL` (crates/yagra-common/src/meraki.rs). The order is cadence order,
    // which is also the order the cadence dialog renders its interval fields in.
    expect([...MERAKI_TIERS]).toEqual([
      'availability',
      'uplink',
      'switch_ports',
      'traffic',
      'inventory',
    ]);
    expect(new Set(MERAKI_TIERS).size).toBe(MERAKI_TIERS.length);
  });
});

describe('SELECTABLE_MERAKI_TIERS', () => {
  it('is a subset of the full tier list', () => {
    for (const tier of SELECTABLE_MERAKI_TIERS) expect(MERAKI_TIERS).toContain(tier);
  });

  it('omits exactly the tiers that are not recurring collects', () => {
    // Equality, not `not.toContain('inventory')`: a fifth tier nobody wires into the dialog fails
    // here, which is the whole point — the inline `['availability', 'uplink', 'traffic']` this
    // replaced could have stayed three long forever with nothing noticing.
    const omitted = MERAKI_TIERS.filter(
      (tier) => !(SELECTABLE_MERAKI_TIERS as readonly string[]).includes(tier),
    );
    expect(omitted).toEqual(['inventory']);
  });
});

describe('the tier an organization cannot go without (ADR-164 決定 17)', () => {
  it('is availability, and the dialog draws a checkbox for every other selectable tier', () => {
    // Availability is the only tier that says whether a device is up. Equality on the rest: a new
    // selectable tier has to land on one side or the other on purpose.
    expect(REQUIRED_MERAKI_TIER).toBe('availability');
    expect([...OPTIONAL_MERAKI_TIERS]).toEqual(['uplink', 'switch_ports', 'traffic']);
    expect([REQUIRED_MERAKI_TIER, ...OPTIONAL_MERAKI_TIERS]).toEqual([...SELECTABLE_MERAKI_TIERS]);
  });

  it('is in every save, whatever was ticked — the server refuses a cadence without it', () => {
    expect(tiersToSave([])).toEqual(['availability']);
    expect(tiersToSave(['traffic'])).toEqual(['availability', 'traffic']);
    expect(tiersToSave(new Set(['uplink', 'availability']))).toEqual(['availability', 'uplink']);
  });

  it('carries a stored tier the dialog does not draw, and sends none twice', () => {
    expect(tiersToSave(['inventory', 'uplink', 'inventory'])).toEqual([
      'availability',
      'inventory',
      'uplink',
    ]);
  });
});

describe('tierList', () => {
  const t = ((key: string) => key) as unknown as TFunction;

  it('joins the tiers a org collects', () => {
    expect(tierList(['availability', 'uplink'], t)).toBe('meraki.tier.availability, meraki.tier.uplink');
  });

  it('gives an org with no tiers a sentence, not an empty cell', () => {
    // An org added but not yet configured is a real state, and a blank cell reads as a broken row.
    expect(tierList([], t)).toBe('meraki.tiersNone');
  });
});
