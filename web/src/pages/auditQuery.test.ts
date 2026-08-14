// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the audit-log query builder (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import { defaultFilters, isAnyFiltered, specColumns } from '../lib/columnFilter';
import { encodeCondition } from '../lib/filterCondition';
import {
  appendPage,
  auditFilters,
  nextCursor,
  exportUrl,
  PAGE_SIZE,
  queryFor,
} from './auditQuery';

const NOW = Date.parse('2026-08-12T00:00:00.000Z');
const t = ((k: string) => k) as unknown as TFunction;
const COLUMNS = specColumns(auditFilters(t));
/** The state the screen opens with — derived from the specs, exactly as the page derives it. */
const DEFAULTS = defaultFilters(COLUMNS);
const query = (over: Record<string, string> = {}, before: string | null = null) =>
  queryFor(COLUMNS, { ...DEFAULTS, ...over }, before, NOW);

describe('queryFor', () => {
  it('sends only the page size when nothing is filtered', () => {
    // Every unset filter must be absent from the query string, not present and empty: `action=`
    // reaches the backend as an unknown action and 400s a request nobody meant to filter.
    expect(query()).toEqual({
      q: undefined,
      action: undefined,
      status: undefined,
      since: undefined,
      before: undefined,
      limit: PAGE_SIZE,
    });
  });

  it('maps each control to its own parameter', () => {
    expect(
      query({
        q: encodeCondition({ term: 'admin', mode: 'contains', not: false }),
        action: 'delete',
        status: 'client',
        range: '7d',
      }),
    ).toEqual({
      q: 'admin',
      action: 'delete',
      status: 'client',
      since: '2026-08-05T00:00:00.000Z',
      before: undefined,
      limit: PAGE_SIZE,
    });
  });

  it('drops a token the column does not offer instead of forwarding it', () => {
    // 🚨 The trap decision AA had to close. `readFilterParams` passes a hand-typed value through
    // untouched — a stale bookmark must open the default view, not a broken control — so if
    // `queryFor` forgot `normalizeSets`, `?action=bogus` would reach an endpoint that rejects
    // unknown actions and the screen would show a 400 for a filter nobody set.
    expect(query({ action: 'bogus' }).action).toBeUndefined();
    expect(query({ status: 'server,bogus' }).status).toBe('server');
    expect(query({ action: 'delete,bogus' }).action).toBe('delete');
  });

  it('falls back to the default window for a preset a stale URL names', () => {
    // Not to "all time": the widening answer is the dangerous one, which is why the seconds come
    // off the spec's own presets through `decodeRange` rather than from a raw token.
    expect(query({ range: 'last-fortnight' }).since).toBeUndefined();
    expect(query({ range: '24h' }).since).toBe('2026-08-11T00:00:00.000Z');
  });

  it('keeps the range while the cursor advances', () => {
    // The two are different things: `since` is the window the operator asked to see, `before` is
    // where this page starts. Conflating them would reset the window on every scroll.
    const page2 = query({ range: '24h' }, '2026-08-11T12:00:00.000Z');
    expect(page2.since).toBe('2026-08-11T00:00:00.000Z');
    expect(page2.before).toBe('2026-08-11T12:00:00.000Z');
  });

  it('drops a whitespace-only search rather than sending it', () => {
    expect(query({ q: '   ' }).q).toBeUndefined();
  });

  it('sends the term of a text condition, never its encoding', () => {
    // `q` has neither a regex nor a negated form on the wire, so the mode and the NOT stay in the
    // browser. A leading `!` in the term is escaped in the stored value and must arrive unescaped.
    expect(query({ q: encodeCondition({ term: '!admin', mode: 'contains', not: false }) }).q).toBe(
      '!admin',
    );
  });
});

describe('the empty state discriminator', () => {
  it('is false for the default view and flips for every column', () => {
    // ⚠️ Keyed off the filters, never off `rows.length`: with the predicate in SQL, a filtered
    // query that legitimately returns zero is indistinguishable from an empty log, and the screen
    // would say "no audit entries" while a filter is hiding them.
    expect(isAnyFiltered(COLUMNS, DEFAULTS)).toBe(false);
    for (const c of COLUMNS) {
      const state = { ...DEFAULTS, [c.key]: c.key === 'range' ? '24h' : 'x' };
      expect(isAnyFiltered(COLUMNS, state), `${c.key} did not register as a filter`).toBe(true);
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
  const url = (over: Record<string, string> = {}) =>
    exportUrl(COLUMNS, { ...DEFAULTS, ...over }, AT);

  it('carries the filter the page is showing', () => {
    // The whole point of the change that moved this server-side: Export and the list must answer
    // questions about the same set. The button used to write out the rows that happened to load.
    const params = new URLSearchParams(
      url({
        q: encodeCondition({ term: 'admin', mode: 'contains', not: false }),
        action: 'delete',
        status: 'client',
        range: '30d',
      }).split('?')[1],
    );
    expect(params.get('q')).toBe('admin');
    expect(params.get('action')).toBe('delete');
    expect(params.get('status')).toBe('client');
    expect(params.get('since')).toBe(new Date(AT - 30 * 86_400_000).toISOString());
  });

  it('sends no cursor and no limit', () => {
    // An export is not paged. "The second page of an export" is not something an operator can act
    // on, and offering it would let a caller export one page and believe it was the answer.
    const params = new URLSearchParams(url({ q: 'x' }).split('?')[1]);
    expect(params.has('before')).toBe(false);
    expect(params.has('limit')).toBe(false);
  });

  it('sends nothing at all for the default view', () => {
    // `action=` would reach the API edge as an empty string and be rejected as an unknown action —
    // a filter nobody set turning a download into a 400.
    expect(url()).toBe('/api/v1/audit/export.csv');
  });

  it('omits an unset field rather than sending it empty', () => {
    const params = new URLSearchParams(url({ action: 'login' }).split('?')[1]);
    expect([...params.keys()]).toEqual(['action']);
  });
});
