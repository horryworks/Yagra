// SPDX-License-Identifier: AGPL-3.0-only
// The cadence dialog's tier checkboxes, pinned to the backend's full tier list.

import { describe, expect, it } from 'vitest';
import { MERAKI_TIERS, SELECTABLE_MERAKI_TIERS } from './merakiTiers';

describe('MERAKI_TIERS', () => {
  it('lists every backend tier exactly once, in cadence order', () => {
    // Mirrors `MerakiTier::ALL` (crates/yagra-common/src/meraki.rs). The order is cadence order,
    // which is also the order the cadence dialog renders its interval fields in.
    expect([...MERAKI_TIERS]).toEqual(['availability', 'uplink', 'traffic', 'inventory']);
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
