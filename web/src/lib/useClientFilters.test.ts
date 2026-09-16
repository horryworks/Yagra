// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
//
// The filter row for a list already in the browser (ADR-053 Inc.3) — the hook eight screens share.
//
// The pieces underneath are each covered elsewhere: the predicate in `filterPredicate.test.ts`, the
// counting rule in `filterCounts.test.ts`, the URL codec in `columnFilter.test.ts`. What is only
// testable here is the wiring, and the wiring is what Inc.3 existed to stop getting wrong: eight
// hand-written copies of the same memo dependencies, where a missing `filters` leaves the table
// showing the *previous* filter's rows. That failure has no error and no empty state — it reads as
// the control being slow rather than broken, so it survives a screen test and a demo.
//
// Where the state lives gets its own tests. It is always the URL since ADR-153 — before that a
// per-screen `url` flag defaulted to component state, and every screen that left it off lost its
// filters on a reload. A route with two tables gives the second one a `prefix`, and getting that
// wrong is silent: two tables writing the same key filter each other.

import { createElement, type ReactNode } from 'react';
import { act, renderHook } from '@testing-library/react';
import { MemoryRouter, useLocation } from 'react-router-dom';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ColumnFilterSpec } from './columnFilter';
import { useClientFilters } from './useClientFilters';

interface Row {
  id: string;
  kind: 'syslog' | 'trap';
  sev: 'critical' | 'warning';
  name: string;
}

// Module scope: the hook memoizes on `columns`, and a fixture rebuilt per render would recompute
// `shown` and every facet on every render — the thing these memos exist to prevent.
const COLUMNS: { key: string; filter?: ColumnFilterSpec<Row> }[] = [
  { key: 'id' }, // no filter — must not appear in `filterCols`
  {
    key: 'kind',
    filter: {
      kind: 'enum',
      options: [
        { value: 'syslog', label: 'Syslog' },
        { value: 'trap', label: 'Trap' },
      ],
      allLabel: 'All kinds',
      counts: 'client',
      readValue: (r) => r.kind,
    },
  },
  {
    key: 'sev',
    filter: {
      kind: 'enum',
      options: [
        { value: 'critical', label: 'Critical' },
        { value: 'warning', label: 'Warning' },
      ],
      allLabel: 'All severities',
      counts: 'client',
      readValue: (r) => r.sev,
    },
  },
  { key: 'q', filter: { kind: 'text', modes: ['contains'], readText: (r) => [r.name] } },
];

const ROWS: Row[] = [
  { id: 'a', kind: 'syslog', sev: 'critical', name: 'sw-core' },
  { id: 'b', kind: 'syslog', sev: 'warning', name: 'sw-edge' },
  { id: 'c', kind: 'trap', sev: 'critical', name: 'rt-1' },
  { id: 'd', kind: 'trap', sev: 'warning', name: 'rt-2' },
];

const ids = (rows: Row[]) => rows.map((r) => r.id);

const wrapper = ({ children }: { children: ReactNode }) =>
  createElement(MemoryRouter, { initialEntries: ['/events'] }, children);

/** The hook, plus the query string so where the state went is observable. */
const mount = (opts?: { prefix?: string }, rows: readonly Row[] = ROWS) =>
  renderHook(() => ({ ...useClientFilters(COLUMNS, rows, opts), search: useLocation().search }), {
    wrapper,
  });

describe('useClientFilters', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(Date.parse('2026-08-14T10:00:00Z'));
  });
  afterEach(() => vi.useRealTimers());

  it('keeps only the columns that carry a filter, in column order', () => {
    const { result } = mount();
    expect(result.current.filterCols.map((c) => c.key)).toEqual(['kind', 'sev', 'q']);
  });

  it('starts unfiltered, showing every row', () => {
    const { result } = mount();
    expect(result.current.filters).toEqual({ kind: '', sev: '', q: '' });
    expect(ids(result.current.shown)).toEqual(['a', 'b', 'c', 'd']);
    expect(result.current.anyFiltered).toBe(false);
  });

  it('applies the predicate as soon as the state changes', () => {
    // The Inc.3 regression shape: a stale `shown` shows the previous filter's rows. Asserting the
    // rows immediately after the write is what makes a missing memo dependency visible at all.
    const { result } = mount();
    act(() => {
      result.current.setFilters({ ...result.current.filters, kind: 'syslog' });
    });
    expect(ids(result.current.shown)).toEqual(['a', 'b']);
    expect(result.current.anyFiltered).toBe(true);

    act(() => {
      result.current.setFilters({ ...result.current.filters, kind: 'syslog', sev: 'warning' });
    });
    expect(ids(result.current.shown)).toEqual(['b']);
  });

  it('narrows on a text column too', () => {
    const { result } = mount();
    act(() => {
      result.current.setFilters({ ...result.current.filters, q: 'sw-' });
    });
    expect(ids(result.current.shown)).toEqual(['a', 'b']);
  });

  it("counts a column's options over the rows passing every OTHER filter", () => {
    // The rule the whole facet feature turns on. Count the *displayed* rows instead and selecting
    // `syslog` immediately reports `trap: 0` — telling the operator that the thing they might
    // switch to is empty, when it has as many rows as the one they picked.
    const { result } = mount();
    act(() => {
      result.current.setFilters({ ...result.current.filters, kind: 'syslog' });
    });

    expect(result.current.counts.kind).toEqual({ syslog: 2, trap: 2 });
    // …while a *different* column's counts DO respect the kind filter: those two rows are what
    // picking a severity would be narrowing.
    expect(result.current.counts.sev).toEqual({ critical: 1, warning: 1 });
  });

  it('counts nothing for a column with no options to decorate', () => {
    // A text column has no list to put numbers against, and asking for one would be a pass over
    // every row per render for nothing.
    const { result } = mount();
    expect(Object.keys(result.current.counts).sort()).toEqual(['kind', 'sev']);
  });

  it('keeps an option that counts zero, so it can still be un-selected', () => {
    const { result } = mount({}, [ROWS[0], ROWS[1]]); // syslog only
    expect(result.current.counts.kind).toEqual({ syslog: 2, trap: 0 });
  });

  it('clear() resets every column at once', () => {
    const { result } = mount();
    act(() => {
      result.current.setFilters({ kind: 'trap', sev: 'critical', q: 'rt' });
    });
    expect(ids(result.current.shown)).toEqual(['c']);

    act(() => {
      result.current.clear();
    });
    expect(result.current.filters).toEqual({ kind: '', sev: '', q: '' });
    expect(ids(result.current.shown)).toEqual(['a', 'b', 'c', 'd']);
    expect(result.current.anyFiltered).toBe(false);
  });

  // ── Where the state lives ───────────────────────────────────────────────────────────────────

  it('puts the state in the URL, so a reload keeps it', () => {
    const { result } = mount();
    act(() => {
      result.current.setFilters({ ...result.current.filters, kind: 'trap' });
    });
    expect(result.current.search).toBe('?kind=trap');
    expect(ids(result.current.shown)).toEqual(['c', 'd']);
  });

  it('reads the query string it arrives with', () => {
    const { result } = renderHook(() => useClientFilters(COLUMNS, ROWS), {
      wrapper: ({ children }: { children: ReactNode }) =>
        createElement(MemoryRouter, { initialEntries: ['/events?kind=trap'] }, children),
    });
    expect(ids(result.current.shown)).toEqual(['c', 'd']);
  });

  it('writes a prefixed table under its own keys, so two tables on one route do not filter each other', () => {
    // The shape of Alerts ▸ Notification delivery: two tables, both with the same column keys. Each
    // one gets its prefix, and narrowing one must leave the other showing every row.
    const { result } = renderHook(
      () => ({
        a: useClientFilters(COLUMNS, ROWS, { prefix: 'a.' }),
        b: useClientFilters(COLUMNS, ROWS, { prefix: 'b.' }),
        search: useLocation().search,
      }),
      { wrapper },
    );
    act(() => {
      result.current.a.setFilters({ ...result.current.a.filters, kind: 'trap' });
    });
    expect(result.current.search).toBe('?a.kind=trap');
    expect(ids(result.current.a.shown)).toEqual(['c', 'd']);
    expect(ids(result.current.b.shown), 'the other table picked up the filter').toEqual(['a', 'b', 'c', 'd']);
  });

  it('reads a prefixed key and ignores the bare one beside it', () => {
    // `/nodes?kind=device&events.kind=trap`: the tree's `kind` and the Events tab's, on one URL.
    const { result } = renderHook(() => useClientFilters(COLUMNS, ROWS, { prefix: 'events.' }), {
      wrapper: ({ children }: { children: ReactNode }) =>
        createElement(MemoryRouter, { initialEntries: ['/nodes?kind=syslog&events.kind=trap'] }, children),
    });
    expect(ids(result.current.shown)).toEqual(['c', 'd']);
  });

  it('clear() on a prefixed table removes its keys and nothing else', () => {
    const { result } = renderHook(
      () => ({ ...useClientFilters(COLUMNS, ROWS, { prefix: 'b.' }), search: useLocation().search }),
      {
        wrapper: ({ children }: { children: ReactNode }) =>
          createElement(MemoryRouter, { initialEntries: ['/x?kind=syslog&b.kind=trap&b.q=rt'] }, children),
      },
    );
    act(() => result.current.clear());
    expect(result.current.search).toBe('?kind=syslog');
  });

  it('holds one instant for relative ranges', () => {
    // Re-reading the clock per render is what drops rows between paged requests.
    const { result, rerender } = mount();
    const pinned = result.current.nowMs;
    vi.setSystemTime(Date.now() + 60_000);
    rerender();
    act(() => {
      result.current.setFilters({ ...result.current.filters, q: 'sw' });
    });
    expect(result.current.nowMs).toBe(pinned);
  });
});
