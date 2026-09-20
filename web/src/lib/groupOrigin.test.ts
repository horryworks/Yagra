// SPDX-License-Identifier: AGPL-3.0-only
// Which folders the tree marks as kept by an integration (ADR-164 Inc.7).

import { describe, expect, it } from 'vitest';
import { GROUP_ORIGINS, type NodeGroup } from '../types/api';
import { GROUP_ORIGIN_BADGES, groupOriginOf } from './groupOrigin';

describe('groupOriginOf', () => {
  it('passes through every origin this build knows', () => {
    for (const origin of GROUP_ORIGINS) expect(groupOriginOf({ origin })).toBe(origin);
    // Named as well, so an emptied list cannot make the loop above pass by doing nothing.
    expect(groupOriginOf({ origin: 'meraki' })).toBe('meraki');
    expect(groupOriginOf({ origin: 'netbox' })).toBe('netbox');
  });

  it('marks nothing on a folder a person made', () => {
    expect(groupOriginOf({ origin: null })).toBeNull();
    // An older core does not send the field at all.
    expect(groupOriginOf({ origin: undefined })).toBeNull();
    expect(groupOriginOf({})).toBeNull();
  });

  it('marks nothing for an origin a newer core sends and this build does not know', () => {
    // The alternative is an empty badge titled `tree.origin.servicenow`: `GROUP_ORIGIN_BADGES`
    // has no such key, and neither locale has the string.
    const fromNewerCore = { origin: 'servicenow' } as unknown as Pick<NodeGroup, 'origin'>;
    expect(groupOriginOf(fromNewerCore)).toBeNull();
    expect(groupOriginOf({ origin: '' } as unknown as Pick<NodeGroup, 'origin'>)).toBeNull();
  });
});

describe('GROUP_ORIGIN_BADGES', () => {
  it('has a badge for exactly the known origins', () => {
    // `Record<GroupOrigin, …>` already makes a missing one a compile error; this is the runtime
    // half, against the array the Rust side is compared with.
    expect(Object.keys(GROUP_ORIGIN_BADGES).sort()).toEqual([...GROUP_ORIGINS].sort());
  });

  it('writes the brand names the way the brands do', () => {
    expect(GROUP_ORIGIN_BADGES.meraki).toBe('Meraki');
    expect(GROUP_ORIGIN_BADGES.netbox).toBe('NetBox');
  });
});
