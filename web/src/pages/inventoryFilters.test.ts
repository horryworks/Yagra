// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Nodes tree's state / kind / pool filters (no DOM — Vitest node env).
//
// Rewritten for ADR-053 Inc.6, where all three became sets. Every property the single-valued
// version was tested for still holds; what is new is the set spelling, and one property that only
// starts to matter once a value can carry several tokens — that the URL settles to a stable order.

import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import { DISPLAY_ORDER, PROBLEM_STATES } from '../lib/nodeState';
import { NODE_KINDS } from '../types/api';
import { defaultFilters, type FilterState } from '../lib/columnFilter';
import {
  ATTENTION_STATES,
  inventoryColumns,
  inventoryFilterLabels,
  inventoryKey,
  inventoryQuery,
  isAttentionOnly,
  isInventoryFiltered,
  NODE_STATE_FILTERS,
  readInventoryFilters,
  toggleAttention,
  truncationNotice,
  writeInventoryFilters,
} from './inventoryFilters';

const t = ((k: string) => k) as unknown as TFunction;
const POOLS = [{ name: 'default' }, { name: 'tokyo' }];
const COLS = inventoryColumns(t, POOLS);
const DEFAULTS = defaultFilters(COLS);
const f = (over: FilterState): FilterState => ({ ...DEFAULTS, ...over });

describe('the offered vocabularies', () => {
  it('offers every node state, from the one list that enumerates the union', () => {
    // A state missing here is one the operator cannot filter for, and nothing else would say so —
    // `NodeState` is a generated union with no runtime form of its own.
    expect(NODE_STATE_FILTERS).toEqual(DISPLAY_ORDER);
    expect(NODE_STATE_FILTERS).toHaveLength(6);
    const state = COLS.find((c) => c.key === 'state')?.filter;
    expect(state?.kind === 'enum' && state.options.map((o) => o.value)).toEqual([
      ...NODE_STATE_FILTERS,
    ]);
  });

  it('offers every node kind', () => {
    expect(NODE_KINDS).toEqual(['wireless_ap', 'meraki', 'url', 'dns', 'device']);
    const kind = COLS.find((c) => c.key === 'kind')?.filter;
    expect(kind?.kind === 'enum' && kind.options.map((o) => o.value)).toEqual([...NODE_KINDS]);
  });

  it('takes its pool options from the deployment, not from an enum', () => {
    // Pools are operator-named, which is why the column list is a function of the fetched list.
    const pool = COLS.find((c) => c.key === 'pool')?.filter;
    expect(pool?.kind === 'enum' && pool.options.map((o) => o.value)).toEqual(['default', 'tokyo']);
  });

  it('labels every column, so the bar never shows a raw key', () => {
    const labels = inventoryFilterLabels(t);
    for (const c of COLS) expect(labels[c.key]).toBeTruthy();
  });

  it('declares no row accessor, because this list is filtered server-side', () => {
    // ⚠️ These carried `readValue: () => null` until the accessor became optional (ADR-053 Inc.8),
    // and that placeholder was worse than none: `null` means "this row has no value", so a
    // predicate built over it would reject every row rather than skip the column. Absence is what
    // says server-side now. If one of these ever gains an accessor, someone will filter locally —
    // and a local filter here answers "of the folders you have opened", a different question with
    // no sign that it changed.
    for (const c of COLS) {
      expect(c.filter.kind).toBe('enum');
      if (c.filter.kind !== 'enum') continue;
      expect(c.filter.readValue).toBeUndefined();
    }
  });
});

describe('inventoryQuery', () => {
  it('sends nothing at all for the default filters', () => {
    expect(inventoryQuery(DEFAULTS)).toEqual({
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

  it('passes a set through in the joined spelling the API takes', () => {
    // The API's own spelling since Inc.6, so there is nothing to re-encode at the last moment.
    expect(inventoryQuery(f({ state: 'warning,critical,unreachable' })).state).toBe(
      'warning,critical,unreachable',
    );
  });
});

describe('isInventoryFiltered', () => {
  it('is false at the defaults and flips for every field', () => {
    expect(isInventoryFiltered(DEFAULTS)).toBe(false);
    for (const c of COLS) {
      // Every field: a filter added without a case here would leave the tree showing "nothing
      // matches" while rows are hidden, which is the failure `ui-conventions` names.
      expect(isInventoryFiltered(f({ [c.key]: 'x' }))).toBe(true);
    }
  });
});

describe('inventoryKey', () => {
  it('is equal for equal values and different for different ones', () => {
    // It exists to be an effect dependency: the filters object is rebuilt from the URL on every
    // render, so keying on identity would re-issue the search on every unrelated re-render.
    expect(inventoryKey(f({ kind: 'url' }))).toBe(inventoryKey(f({ kind: 'url' })));
    expect(inventoryKey(f({ kind: 'url' }))).not.toBe(inventoryKey(f({ kind: 'dns' })));
    expect(inventoryKey(DEFAULTS)).not.toBe(inventoryKey(f({ pool: 'tokyo' })));
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
    writeInventoryFilters(COLS, params, filters);
    expect(readInventoryFilters(COLS, params)).toEqual(filters);
  });

  it('round-trips a set, and settles it to the option order', () => {
    // The joined value is `inventoryKey`'s input, so a value that varied with click order would
    // re-issue the server search for a filter nobody changed.
    const params = new URLSearchParams('state=unknown,ok');
    const read = readInventoryFilters(COLS, params);
    expect(read.state).toBe('ok,unknown');
    expect(inventoryKey(read)).toBe(
      inventoryKey(readInventoryFilters(COLS, new URLSearchParams('state=ok,unknown'))),
    );
  });

  it('writes no key at the default, so "?" always means something is narrowing', () => {
    const params = new URLSearchParams('sel=node:abc');
    writeInventoryFilters(COLS, params, DEFAULTS);
    expect(params.toString()).toBe('sel=node%3Aabc');
  });

  it('clears a key when the filter goes back to its default', () => {
    const params = new URLSearchParams('state=critical&kind=dns&pool=tokyo');
    writeInventoryFilters(COLS, params, DEFAULTS);
    expect(params.has('state')).toBe(false);
    expect(params.has('kind')).toBe(false);
    expect(params.has('pool')).toBe(false);
  });

  it('drops a state or kind token this build does not know', () => {
    // A bookmark written by a newer build must not break the page. Deliberately the opposite of
    // the API edge, which rejects an unknown token — there, widening would answer a different
    // question than the one asked; here the operator can see the control did not take.
    const params = new URLSearchParams('state=on-fire&kind=switch');
    expect(readInventoryFilters(COLS, params)).toMatchObject({ state: '', kind: '' });
    // …and keeps the half it does know.
    expect(readInventoryFilters(COLS, new URLSearchParams('state=on-fire,ok')).state).toBe('ok');
  });

  it('keeps a pool name it has never heard of', () => {
    // ⚠️ The one column that is NOT validated against its options, and deliberately: the pool list
    // is fetched, so a link opened before that request lands would have its pool filter silently
    // erased. State and kind are compile-time vocabularies with no such window.
    const early = inventoryColumns(t, []);
    expect(readInventoryFilters(early, new URLSearchParams('pool=tokyo')).pool).toBe('tokyo');
  });

  it('leaves other query keys alone', () => {
    const params = new URLSearchParams('sel=group:g1&tab=overview');
    writeInventoryFilters(COLS, params, f({ kind: 'url' }));
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

describe('the "Needs attention" preset (ADR-163)', () => {
  it('selects the same three states the page header counts, in the filter\'s own order', () => {
    // The SET is what makes the button agree with the "N need attention" above it — one enumerated
    // by hand would quietly disagree, and neither surface would look wrong on its own.
    // ⚠️ The ORDER is pinned here only so the array and the URL read alike; `encodeSet` is what
    // actually settles the URL, and it re-orders whatever it is handed. So this assertion is the
    // only thing watching the order, and reversing the array breaks nothing else — do not read it
    // as proof that the order is load-bearing.
    expect([...ATTENTION_STATES]).toEqual([...PROBLEM_STATES].sort((a, b) =>
      NODE_STATE_FILTERS.indexOf(a) - NODE_STATE_FILTERS.indexOf(b),
    ));
    expect([...ATTENTION_STATES]).toEqual(['warning', 'critical', 'unreachable']);
    // Down is `unreachable`, never `critical` — dropping it would take the most urgent rows out of
    // the one control an operator reaches for during an outage.
    expect(ATTENTION_STATES).toContain('unreachable');
    // And the neutral states stay out: `maintenance` is silenced on purpose, `unknown` says
    // "investigate", not "page someone" (`NodeState::is_problem`).
    expect(ATTENTION_STATES).not.toContain('maintenance');
    expect(ATTENTION_STATES).not.toContain('unknown');
    expect(ATTENTION_STATES).not.toContain('ok');
  });

  it('turns on from nothing and off again', () => {
    const on = toggleAttention(f({}));
    expect(on.state).toBe('warning,critical,unreachable');
    expect(isAttentionOnly(on)).toBe(true);

    const off = toggleAttention(on);
    expect(off.state).toBe('');
    expect(isAttentionOnly(off)).toBe(false);
  });

  it('replaces the chosen states rather than adding to them', () => {
    // A preset, not a fourth filter: "everything that is not healthy" plus `ok` is a question
    // nobody asked, and it would put every healthy node back on screen under a lit button.
    const on = toggleAttention(f({ state: 'ok' }));
    expect(on.state).toBe('warning,critical,unreachable');
  });

  it('leaves every other column alone, both ways', () => {
    const before = f({ state: 'ok', kind: 'url', pool: 'tokyo' });
    const on = toggleAttention(before);
    expect(on.kind).toBe('url');
    expect(on.pool).toBe('tokyo');
    const off = toggleAttention(on);
    expect(off.kind).toBe('url');
    expect(off.pool).toBe('tokyo');
  });

  it('reads a hand-typed URL in any order as pressed', () => {
    // `aria-pressed` compares sets, not strings. A link someone typed — or one an older build
    // wrote — asks the same question, and a button left unlit above a tree it has narrowed is the
    // control disagreeing with the screen.
    expect(isAttentionOnly(f({ state: 'critical,warning,unreachable' }))).toBe(true);
    expect(isAttentionOnly(f({ state: 'unreachable,critical,warning' }))).toBe(true);
  });

  it('is not pressed for a subset, a superset, or a different set of the same size', () => {
    // The three ways an equality check written as "every attention state is chosen" or as
    // "length matches" would go wrong on its own.
    expect(isAttentionOnly(f({ state: 'warning,critical' }))).toBe(false);
    expect(isAttentionOnly(f({ state: 'warning,critical,unreachable,ok' }))).toBe(false);
    expect(isAttentionOnly(f({ state: 'ok,unknown,maintenance' }))).toBe(false);
    expect(isAttentionOnly(f({}))).toBe(false);
  });

  it('survives the URL round trip it will actually take', () => {
    // The preset writes a `FilterState`; the page writes that to the URL and reads it back on the
    // next render. If those two disagreed the button would flicker off the moment it was pressed.
    const params = new URLSearchParams();
    writeInventoryFilters(COLS, params, toggleAttention(f({})));
    expect(params.get('state')).toBe('warning,critical,unreachable');
    expect(isAttentionOnly(readInventoryFilters(COLS, params))).toBe(true);
  });

  it('counts as narrowing the tree, so "clear all filters" appears and undoes it', () => {
    // No `extraActive` wiring exists for this button (ADR-163 決定 1): it writes the `state` column,
    // which `isInventoryFiltered` and `ClearFilters` already watch. That is only true while the
    // preset keeps writing that column, which is what this pins.
    const on = toggleAttention(f({}));
    expect(isInventoryFiltered(on)).toBe(true);
    expect(isAttentionOnly(defaultFilters(COLS))).toBe(false);
  });
});
