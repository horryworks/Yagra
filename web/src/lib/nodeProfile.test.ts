// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { nodeProfileOf } from './nodeProfile';
import type { ProfileSummary } from '../types/api';

const profiles: ProfileSummary[] = [
  { id: 'g', name: 'Generic SNMP', category: 'generic-snmp', fallback: true },
  { id: 's', name: 'My switch', category: 'generic-snmp', fallback: false },
];

describe('nodeProfileOf (ADR-231)', () => {
  it('names the profile and carries the fallback mark from the server', () => {
    expect(nodeProfileOf(profiles, 'g')).toEqual({ name: 'Generic SNMP', fallback: true });
    // Same category as the generic one, and still not it: the mark is the server's, not a guess.
    expect(nodeProfileOf(profiles, 's')).toEqual({ name: 'My switch', fallback: false });
  });

  it('shows nothing for a node with no profile, or one the list does not hold', () => {
    expect(nodeProfileOf(profiles, null)).toBeNull();
    expect(nodeProfileOf(profiles, undefined)).toBeNull();
    expect(nodeProfileOf([], 'g')).toBeNull();
  });
});
