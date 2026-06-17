// Pure-helper tests for the Nodes split selection ↔ URL-param round-trip.

import { describe, expect, it } from 'vitest';
import { parseSelection, selectionToParam } from './treeSelection';

describe('selectionToParam', () => {
  it('encodes node and group selections', () => {
    expect(selectionToParam({ kind: 'node', id: 'n1' })).toBe('node:n1');
    expect(selectionToParam({ kind: 'group', id: 'g1' })).toBe('group:g1');
  });

  it('encodes null as null (cleared selection)', () => {
    expect(selectionToParam(null)).toBeNull();
  });
});

describe('parseSelection', () => {
  it('round-trips both kinds', () => {
    for (const sel of [
      { kind: 'node', id: 'abc-123' },
      { kind: 'group', id: 'site-tokyo' },
    ] as const) {
      expect(parseSelection(selectionToParam(sel))).toEqual(sel);
    }
  });

  it('keeps an id that itself contains a colon', () => {
    expect(parseSelection('node:a:b:c')).toEqual({ kind: 'node', id: 'a:b:c' });
  });

  it('returns null for absent, empty, or malformed values', () => {
    expect(parseSelection(null)).toBeNull();
    expect(parseSelection('')).toBeNull();
    expect(parseSelection('node')).toBeNull(); // no separator
    expect(parseSelection('node:')).toBeNull(); // empty id
    expect(parseSelection(':n1')).toBeNull(); // empty kind
    expect(parseSelection('widget:x')).toBeNull(); // unknown kind
  });
});
