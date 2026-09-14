// SPDX-License-Identifier: AGPL-3.0-only
// Pure-helper tests for the Nodes split selection ↔ URL-param round-trip.

import { describe, expect, it } from 'vitest';
import { escapeTarget, nodesPageHref, parseSelection, selectionToParam } from './treeSelection';

describe('selectionToParam', () => {
  it('encodes node and group selections', () => {
    expect(selectionToParam({ kind: 'node', id: 'n1' })).toBe('node:n1');
    expect(selectionToParam({ kind: 'group', id: 'g1' })).toBe('group:g1');
  });

  it('encodes null as null (cleared selection)', () => {
    expect(selectionToParam(null)).toBeNull();
  });
});

describe('nodesPageHref', () => {
  it('opens All nodes on the selection', () => {
    expect(nodesPageHref({ kind: 'group', id: 'g1' })).toBe('/nodes?sel=group%3Ag1');
    expect(nodesPageHref({ kind: 'node', id: 'n1' })).toBe('/nodes?sel=node%3An1');
  });

  it('is the bare page for no selection', () => {
    expect(nodesPageHref(null)).toBe('/nodes');
  });

  it('lands on a URL the page reads back as the same selection, even for an id with a colon', () => {
    for (const sel of [
      { kind: 'group' as const, id: 'a:b' },
      { kind: 'node' as const, id: '00000000-0000-4000-8000-000000000001' },
    ]) {
      const href = new URL(nodesPageHref(sel), 'http://yagra.test');
      expect(href.pathname).toBe('/nodes');
      expect(parseSelection(href.searchParams.get('sel'))).toEqual(sel);
    }
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
