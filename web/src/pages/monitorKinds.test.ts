// SPDX-License-Identifier: AGPL-3.0-only
// The add-node form's kind list, pinned to the union it derives and to the backend's NodeKind.

import { describe, expect, it } from 'vitest';
import {
  ADDABLE_KINDS,
  isAddableKind,
  MONITOR_KINDS,
  monitorKind,
  targetFilled,
  type MonitorTargets,
} from './monitorKinds';
import { NODE_KINDS } from '../types/api';

const empty: MonitorTargets = { address: '', url: '', dnsName: '' };

describe('MONITOR_KINDS', () => {
  it('covers exactly the addable kinds', () => {
    // `monitorKind` asserts non-null on this lookup; that assertion is only safe while this holds.
    expect(MONITOR_KINDS.map((k) => k.kind).sort()).toEqual([...ADDABLE_KINDS].sort());
  });

  it('gives each kind its own strings', () => {
    // A copy-pasted entry that kept another kind's title would show "Add URL monitor" over the DNS
    // form — the sort of thing that survives review because every field looks plausible.
    for (const field of ['optionKey', 'titleKey', 'errorKey', 'target'] as const) {
      const values = MONITOR_KINDS.map((k) => k[field]);
      expect(new Set(values).size, field).toBe(values.length);
    }
  });

  it('is a subset of the backend node kinds', () => {
    // Not equality: a Meraki node comes from the org collector, so there is nothing to type. When
    // NodeKind grows, this fails and someone decides whether the new kind is hand-addable.
    for (const k of ADDABLE_KINDS) expect(NODE_KINDS).toContain(k);
    expect(ADDABLE_KINDS).not.toContain('meraki');
  });
});

describe('targetFilled', () => {
  it('checks the field that kind is actually built around', () => {
    expect(targetFilled('device', { ...empty, address: '10.0.0.1' })).toBe(true);
    expect(targetFilled('url', { ...empty, url: 'https://example.com' })).toBe(true);
    expect(targetFilled('dns', { ...empty, dnsName: 'example.com' })).toBe(true);
    // A filled field belonging to a *different* kind must not enable the submit button; the form
    // keeps all three in state at once, so this is the mistake a ternary chain makes.
    expect(targetFilled('url', { ...empty, address: '10.0.0.1' })).toBe(false);
    expect(targetFilled('dns', { ...empty, url: 'https://example.com' })).toBe(false);
  });

  it('treats whitespace as empty', () => {
    expect(targetFilled('device', { ...empty, address: '   ' })).toBe(false);
  });
});

describe('isAddableKind', () => {
  it('accepts only the listed kinds', () => {
    for (const k of ADDABLE_KINDS) expect(isAddableKind(k)).toBe(true);
    // The select's value arrives as a string; 'meraki' is a real NodeKind but not addable, and
    // casting it in would build a create call the API has no endpoint for.
    expect(isAddableKind('meraki')).toBe(false);
    expect(isAddableKind('')).toBe(false);
  });
});

describe('monitorKind', () => {
  it('resolves every kind to its own spec', () => {
    for (const k of ADDABLE_KINDS) expect(monitorKind(k).kind).toBe(k);
  });
});
