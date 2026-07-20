// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { isEntityResolved, resolveName } from './EntityName';

describe('EntityName helpers', () => {
  const nodes = [
    { id: 'aaaa-1111', name: 'core-sw-01' },
    { id: 'bbbb-2222', name: 'edge-rtr-02' },
  ];

  it('resolves an id to its human name', () => {
    expect(resolveName(nodes, 'aaaa-1111')).toBe('core-sw-01');
    expect(resolveName(nodes, 'bbbb-2222')).toBe('edge-rtr-02');
  });

  it('falls back to the raw id when the reference is unknown (e.g. deleted)', () => {
    expect(resolveName(nodes, 'zzzz-9999')).toBe('zzzz-9999');
    expect(resolveName([], 'aaaa-1111')).toBe('aaaa-1111');
  });

  it('treats a name as resolved only when it differs from a present id', () => {
    // Name distinct from id ⇒ resolved (show name, id on hover).
    expect(isEntityResolved('core-sw-01', 'aaaa-1111')).toBe(true);
    // Name fell back to the raw id (unresolved) ⇒ not resolved (show the raw handle).
    expect(isEntityResolved('aaaa-1111', 'aaaa-1111')).toBe(false);
    // No id, or an empty id ⇒ not resolved.
    expect(isEntityResolved('some-tag')).toBe(false);
    expect(isEntityResolved('some-tag', '')).toBe(false);
  });
});
