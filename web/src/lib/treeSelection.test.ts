// SPDX-License-Identifier: AGPL-3.0-only
// Pure-helper tests for the Nodes split selection ↔ URL-param round-trip.

import { describe, expect, it } from 'vitest';
import { escapeTarget, parseSelection, selectionToParam } from './treeSelection';

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

describe('escapeTarget', () => {
  it('unwinds the working set before the pane', () => {
    // The batch is what the operator is assembling right now; the pane is what they were reading.
    // Clearing the pane first would throw away work the press was not aimed at.
    expect(escapeTarget(true, true)).toBe('checked');
  });

  it('falls through to the pane when nothing is checked', () => {
    expect(escapeTarget(false, true)).toBe('selection');
  });

  it('clears the working set even with no pane open', () => {
    expect(escapeTarget(true, false)).toBe('checked');
  });

  it('says so when there is nothing to clear', () => {
    // The caller mounts no listener at all in this case, so answering null is the second line of
    // defence rather than the first.
    expect(escapeTarget(false, false)).toBeNull();
  });
});
