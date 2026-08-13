// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Nodes tree's state / kind / pool filters (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import { DISPLAY_ORDER } from '../lib/nodeState';
import { NODE_KINDS } from '../types/api';
import {
  DEFAULT_INVENTORY_FILTERS,
  inventoryKey,
  inventoryQuery,
  isInventoryFiltered,
  NODE_STATE_FILTERS,
  readInventoryFilters,
  truncationNotice,
  writeInventoryFilters,
  type InventoryFilters,
} from './inventoryFilters';

const f = (over: Partial<InventoryFilters>): InventoryFilters => ({
  ...DEFAULT_INVENTORY_FILTERS,
  ...over,
});

describe('the offered vocabularies', () => {
  it('offers every node state, from the one list that enumerates the union', () => {
    // A state missing here is one the operator cannot filter for, and nothing else would say so —
    // `NodeState` is a generated union with no runtime form of its own.
    expect(NODE_STATE_FILTERS).toEqual(DISPLAY_ORDER);
    expect(NODE_STATE_FILTERS).toHaveLength(6);
  });

  it('offers every node kind', () => {
    expect(NODE_KINDS).toEqual(['meraki', 'url', 'dns', 'device']);
  });
});

describe('inventoryQuery', () => {
  it('sends nothing at all for the default filters', () => {
    expect(inventoryQuery(DEFAULT_INVENTORY_FILTERS)).toEqual({
      state: undefined,
      kind: undefined,
      pool: undefined,
    });
  });

  it('turns an unset field into undefined, never an empty string', () => {
    // `?state=` reaches the API edge as a value it cannot parse, which is a 400 — the mistake
    // `findingsQuery.ts` documents having made.
    const q = inventoryQuery(f({ kind: 'url' }));
    expect(q.kind).toBe('url');
    expect(q.state).toBeUndefined();
    expect(Object.values(q)).not.toContain('');
  });

  it('trims a pool name, and treats whitespace as unset', () => {
    expect(inventoryQuery(f({ pool: '  tokyo  ' })).pool).toBe('tokyo');
    expect(inventoryQuery(f({ pool: '   ' })).pool).toBeUndefined();
  });
});

describe('isInventoryFiltered', () => {
  it('is false at the defaults and flips for every field', () => {
    expect(isInventoryFiltered(DEFAULT_INVENTORY_FILTERS)).toBe(false);
    for (const key of Object.keys(DEFAULT_INVENTORY_FILTERS) as (keyof InventoryFilters)[]) {
      // Every field, driven off the defaults object rather than a hand-written list: a filter
      // added without a case here would leave the tree showing "nothing matches" while rows are
      // hidden, which is the failure `ui-conventions` names.
      const changed = f({ [key]: 'x' } as Partial<InventoryFilters>);
      expect(isInventoryFiltered(changed)).toBe(true);
    }
  });
});

describe('inventoryKey', () => {
  it('is equal for equal values and different for different ones', () => {
    // It exists to be an effect dependency: the filters object is rebuilt from the URL on every
    // render, so keying on identity would re-issue the search on every unrelated re-render.
    expect(inventoryKey(f({ kind: 'url' }))).toBe(inventoryKey(f({ kind: 'url' })));
    expect(inventoryKey(f({ kind: 'url' }))).not.toBe(inventoryKey(f({ kind: 'dns' })));
    expect(inventoryKey(DEFAULT_INVENTORY_FILTERS)).not.toBe(inventoryKey(f({ pool: 'tokyo' })));
  });

  it('does not collide across fields', () => {
    // A naive concatenation would make {kind:'a b'} and {kind:'a', pool:'b'} the same string.
    expect(inventoryKey(f({ pool: 'url' }))).not.toBe(inventoryKey(f({ kind: 'url' })));
  });
});

describe('the URL codec', () => {
  it('round-trips every field', () => {
    const params = new URLSearchParams();
    const filters = f({ state: 'critical', kind: 'dns', pool: 'tokyo' });
    writeInventoryFilters(params, filters);
    expect(readInventoryFilters(params)).toEqual(filters);
  });

  it('writes no key at the default, so "?" always means something is narrowing', () => {
    const params = new URLSearchParams('sel=node:abc');
    writeInventoryFilters(params, DEFAULT_INVENTORY_FILTERS);
    expect(params.toString()).toBe('sel=node%3Aabc');
  });

  it('clears a key when the filter goes back to its default', () => {
    const params = new URLSearchParams('state=critical&kind=dns&pool=tokyo');
    writeInventoryFilters(params, DEFAULT_INVENTORY_FILTERS);
    expect(params.has('state')).toBe(false);
    expect(params.has('kind')).toBe(false);
    expect(params.has('pool')).toBe(false);
  });

  it('falls back to no filter for a value this build does not know', () => {
    // A bookmark written by a newer build must not break the page. Deliberately the opposite of
    // the API edge, which rejects an unknown token — there, widening would answer a different
    // question than the one asked; here the operator can see the control did not take.
    const params = new URLSearchParams('state=on-fire&kind=switch');
    expect(readInventoryFilters(params)).toMatchObject({ state: '', kind: '' });
  });

  it('leaves other query keys alone', () => {
    const params = new URLSearchParams('sel=group:g1&tab=overview');
    writeInventoryFilters(params, f({ kind: 'url' }));
    expect(params.get('sel')).toBe('group:g1');
    expect(params.get('tab')).toBe('overview');
    expect(params.get('kind')).toBe('url');
  });
});

describe('truncationNotice', () => {
  it('says nothing when the answer is complete', () => {
    expect(truncationNotice(false, 500, 500)).toBe('none');
    expect(truncationNotice(false, 0, 500)).toBe('none');
  });

  it('blames the page when the page is full', () => {
    expect(truncationNotice(true, 500, 500)).toBe('page');
    // Defensive: the client cap and the server cap are two numbers and could drift.
    expect(truncationNotice(true, 501, 500)).toBe('page');
  });

  it('blames the scan when the list is short and still incomplete', () => {
    // The case the old `length >= cap` inference got exactly backwards: three rows out of a
    // 5,000-row scan is the *least* complete answer the tree can show, and it would have called
    // it complete.
    expect(truncationNotice(true, 3, 500)).toBe('scan');
    expect(truncationNotice(true, 0, 500)).toBe('scan');
  });
});
