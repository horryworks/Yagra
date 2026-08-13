// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the audit-log query builder (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import {
  appendPage,
  DEFAULT_FILTERS,
  isFiltered,
  nextCursor,
  exportUrl,
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

describe('exportUrl', () => {
  const AT = Date.UTC(2026, 7, 13, 12, 0, 0);

  it('carries the filter the page is showing', () => {
    // The whole point of the change: Export and the list must answer questions about the same set.
    // The button used to write out the rows that happened to be loaded.
    const url = exportUrl(
      { q: 'admin', action: 'delete', status: 'client', range: '30d' },
      AT,
    );
    const params = new URLSearchParams(url.split('?')[1]);
    expect(params.get('q')).toBe('admin');
    expect(params.get('action')).toBe('delete');
    expect(params.get('status')).toBe('client');
    expect(params.get('since')).toBe(new Date(AT - 30 * 86_400_000).toISOString());
  });

  it('sends no cursor and no limit', () => {
    // An export is not paged. "The second page of an export" is not something an operator can act
    // on, and offering it would let a caller export one page and believe it was the answer.
    const params = new URLSearchParams(
      exportUrl({ q: 'x', action: '', status: '', range: 'all' }, AT).split('?')[1],
    );
    expect(params.has('before')).toBe(false);
    expect(params.has('limit')).toBe(false);
  });

  it('sends nothing at all for the default view', () => {
    // `action=` would reach the API edge as an empty string and be rejected as an unknown action —
    // a filter nobody set turning a download into a 400.
    expect(exportUrl(DEFAULT_FILTERS, AT)).toBe('/api/v1/audit/export.csv');
  });

  it('omits an unset field rather than sending it empty', () => {
    const url = exportUrl({ q: '', action: 'login', status: '', range: 'all' }, AT);
    const params = new URLSearchParams(url.split('?')[1]);
    expect([...params.keys()]).toEqual(['action']);
  });
});
