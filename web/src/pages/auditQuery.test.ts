// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the audit-log query builder (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import {
  appendPage,
  DEFAULT_FILTERS,
  isFiltered,
  nextCursor,
  PAGE_SIZE,
  queryFor,
  type AuditFilters,
} from './auditQuery';

const NOW = Date.parse('2026-08-12T00:00:00.000Z');

describe('queryFor', () => {
  it('sends only the page size when nothing is filtered', () => {
    // Every unset filter must be absent from the query string, not present and empty: `action=`
    // reaches the backend as an unknown action and 400s a request nobody meant to filter.
    expect(queryFor(DEFAULT_FILTERS, null, NOW)).toEqual({
      q: undefined,
      action: undefined,
      status: undefined,
      since: undefined,
      before: undefined,
      limit: PAGE_SIZE,
    });
  });

  it('maps each control to its own parameter', () => {
    const f: AuditFilters = { q: 'admin', action: 'delete', status: 'client', range: '7d' };
    expect(queryFor(f, null, NOW)).toEqual({
      q: 'admin',
      action: 'delete',
      status: 'client',
      since: '2026-08-05T00:00:00.000Z',
      before: undefined,
      limit: PAGE_SIZE,
    });
  });

  it('keeps the range while the cursor advances', () => {
    // The two are different things: `since` is the window the operator asked to see, `before` is
    // where this page starts. Conflating them would reset the window on every scroll.
    const f: AuditFilters = { ...DEFAULT_FILTERS, range: '24h' };
    const page2 = queryFor(f, '2026-08-11T12:00:00.000Z', NOW);
    expect(page2.since).toBe('2026-08-11T00:00:00.000Z');
    expect(page2.before).toBe('2026-08-11T12:00:00.000Z');
  });

  it('drops a whitespace-only search rather than sending it', () => {
    expect(queryFor({ ...DEFAULT_FILTERS, q: '   ' }, null, NOW).q).toBeUndefined();
  });
});

describe('isFiltered', () => {
  it('is false for the default view', () => {
    expect(isFiltered(DEFAULT_FILTERS)).toBe(false);
  });

  it('flips for every field, including ones added later', () => {
    // This loop is why `isFiltered` is derived from DEFAULT_FILTERS instead of being a hand-written
    // disjunction: a filter added without its clause would leave the empty state saying "no audit
    // entries" while a filter is hiding them. Nothing here has to be updated to keep that true.
    const changed: Record<keyof AuditFilters, AuditFilters> = {
      q: { ...DEFAULT_FILTERS, q: 'x' },
      action: { ...DEFAULT_FILTERS, action: 'post' },
      status: { ...DEFAULT_FILTERS, status: 'server' },
      range: { ...DEFAULT_FILTERS, range: '24h' },
    };
    for (const key of Object.keys(DEFAULT_FILTERS) as (keyof AuditFilters)[]) {
      expect(isFiltered(changed[key]), `${key} did not register as a filter`).toBe(true);
    }
  });
});

describe('nextCursor', () => {
  const rows = (n: number) => Array.from({ length: n }, (_, i) => ({ at: `t${i}` }));

  it('takes the last row of a full page', () => {
    expect(nextCursor(rows(PAGE_SIZE))).toBe(`t${PAGE_SIZE - 1}`);
  });

  it('stops on a short page', () => {
    // Only correct because the filter is in SQL. While it ran in the browser a full page could
    // still show nothing, so page length said nothing about whether the log had more to give.
    expect(nextCursor(rows(PAGE_SIZE - 1))).toBeNull();
    expect(nextCursor([])).toBeNull();
  });
});

describe('appendPage', () => {
  it('appends in order', () => {
    expect(appendPage([{ id: 'a' }], [{ id: 'b' }])).toEqual([{ id: 'a' }, { id: 'b' }]);
  });

  it('drops a row already held', () => {
    // Two entries written in the same microsecond would otherwise be re-fetched by the `at` cursor,
    // and a duplicate React key is a silent misrender rather than a visible error.
    expect(appendPage([{ id: 'a' }], [{ id: 'a' }, { id: 'b' }])).toEqual([
      { id: 'a' },
      { id: 'b' },
    ]);
  });
});
