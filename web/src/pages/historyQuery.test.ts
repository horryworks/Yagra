// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the alert-history filter state (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import {
  defaultFilters,
  isAnyFiltered,
  readFilterParams,
  reservedKeyCollisions,
  specColumns,
  writeFilterParams,
  type FilterState,
} from '../lib/columnFilter';
import { decodeCondition, encodeCondition } from '../lib/filterCondition';
import {
  historyFilters,
  queryFor,
  readScope,
  resolvedFor,
  writeScope,
  type HistoryColumns,
} from './historyQuery';
import type { ScopeIds } from '../troubleshoot/findingsQuery';
import { PAGE_SIZE } from './historyCursor';

const NOW = Date.parse('2026-08-12T00:00:00.000Z');
const t = ((k: string) => k) as unknown as TFunction;
const COLUMNS: HistoryColumns = specColumns(historyFilters(t));
/** The state the screen opens with — derived from the specs, exactly as the page derives it. */
const DEFAULTS = defaultFilters(COLUMNS);
const NO_SCOPE: ScopeIds = { nodeId: '', groupId: '' };
const query = (
  over: Record<string, string> = {},
  scope: ScopeIds = NO_SCOPE,
  cursor: { before: string; before_id: string } | null = null,
) => queryFor(COLUMNS, { ...DEFAULTS, ...over }, scope, cursor, NOW);
const contains = (term: string) => encodeCondition({ term, mode: 'contains', not: false });
/** What the page holds after the router hands it a query string. */
const read = (qs: string) => readFilterParams(COLUMNS, new URLSearchParams(qs));

describe('resolvedFor', () => {
  it('distinguishes "both" from "fires only"', () => {
    // undefined and false are different requests: undefined omits the parameter, false asks the
    // backend for fires. Collapsing them would silently hide every clear.
    expect(resolvedFor('')).toBeUndefined();
    expect(resolvedFor('fired')).toBe(false);
    expect(resolvedFor('cleared')).toBe(true);
  });
});

describe('queryFor', () => {
  it('sends only the page size when nothing is filtered', () => {
    expect(query()).toEqual({
      limit: PAGE_SIZE,
      severity: undefined,
      state: undefined,
      resolved: undefined,
      acked: undefined,
      metric: undefined,
      node_id: undefined,
      node_q: undefined,
      group_id: undefined,
      since: undefined,
      before: undefined,
      before_id: undefined,
    });
  });

  it('maps each control to its own parameter', () => {
    expect(
      query(
        {
          // Several severities in one request — the point of Inc.4b. The joined spelling is what
          // the API takes, so nothing re-encodes it on the way out.
          severity: 'warning,critical',
          state: 'unreachable',
          phase: 'fired',
          node_q: contains('core-sw'),
          metric: contains('cpu'),
          acked: 'false',
          range: '7d',
        },
        { nodeId: 'n1', groupId: '' },
      ),
    ).toMatchObject({
      severity: 'warning,critical',
      state: 'unreachable',
      resolved: false,
      // Only a chosen value becomes a boolean, and `false` must survive the conversion — a
      // falsy-check here would drop "unacknowledged only", which is the filter an operator
      // actually reaches for.
      acked: false,
      metric: 'cpu',
      node_id: 'n1',
      node_q: 'core-sw',
      group_id: undefined,
      since: '2026-08-05T00:00:00.000Z',
    });
  });

  it('treats both boxes ticked as the unfiltered view, not as a third answer', () => {
    // `phase` and `acked` map onto booleans, so "fired and cleared" is the same request as
    // neither. `undefined` omits the parameter; `false` would ask for fires only.
    expect(query({ phase: 'fired,cleared' }).resolved).toBeUndefined();
    expect(query({ acked: 'true,false' }).acked).toBeUndefined();
    expect(query({ acked: 'true' }).acked).toBe(true);
  });

  it('drops a token the column does not offer instead of forwarding it', () => {
    // 🚨 The trap decision AA had to close: the URL is read without validation on purpose, so
    // `?severity=bogus` would otherwise reach an endpoint that rejects unknown severities. It is
    // also what lets `phaseOf`/`ackedOf` read `picked[0]` as a token they already know.
    expect(query({ severity: 'bogus' }).severity).toBeUndefined();
    expect(query({ severity: 'bogus,critical' }).severity).toBe('critical');
    expect(query({ phase: 'exploded' }).resolved).toBeUndefined();
    expect(query({ acked: 'maybe' }).acked).toBeUndefined();
  });

  it('falls back to the default window for a preset a stale URL names', () => {
    // Not to a *narrower* one and not to a wider one by accident: the length is looked up in the
    // spec's own presets after `decodeRange` has replaced anything unknown with the default.
    expect(query({ range: 'last-fortnight' }).since).toBeUndefined();
    expect(query({ range: '24h' }).since).toBe('2026-08-11T00:00:00.000Z');
  });

  it('keeps the window while the cursor advances', () => {
    // `since` is what the operator asked to see; the cursor is where this page starts. Conflating
    // them would reset the window on every scroll.
    const page2 = query({ range: '24h' }, NO_SCOPE, { before: 'ts', before_id: 'id' });
    expect(page2.since).toBe('2026-08-11T00:00:00.000Z');
    expect(page2.before).toBe('ts');
    expect(page2.before_id).toBe('id');
  });
});

describe('the empty state discriminator', () => {
  it('is false for the default view and flips for every column', () => {
    // ⚠️ Must not be replaced by a `rows.length` check: with the filter in SQL, a filtered query
    // that legitimately returns zero is indistinguishable from an empty log. Derived from the
    // specs, so a column added without a clause cannot slip past it.
    expect(isAnyFiltered(COLUMNS, DEFAULTS)).toBe(false);
    for (const c of COLUMNS) {
      const state = { ...DEFAULTS, [c.key]: c.key === 'range' ? '24h' : 'x' };
      expect(isAnyFiltered(COLUMNS, state), `${c.key} did not register as a filter`).toBe(true);
    }
  });
});

describe('URL round trip', () => {
  // The codec is the shared one now (`readFilterParams`/`writeFilterParams` through
  // `useFilterParams`). The keys did not move — a column key **is** its query key, and these were
  // already named after the parameters this screen shipped with — so saved links still resolve.
  const write = (state: FilterState, scope: ScopeIds = NO_SCOPE) => {
    const p = new URLSearchParams();
    writeFilterParams(COLUMNS, p, state);
    writeScope(scope)(p);
    return p;
  };

  it('restores every filter from the query string', () => {
    const state: FilterState = {
      ...DEFAULTS,
      severity: 'warning,critical',
      state: 'critical',
      phase: 'cleared',
      node_q: contains('core-sw'),
      metric: contains('cpu_util'),
      acked: 'true',
      range: '30d',
    };
    const scope: ScopeIds = { nodeId: 'n1', groupId: 'g1' };
    const p = write(state, scope);
    expect(read(p.toString())).toEqual(state);
    expect(readScope(p)).toEqual(scope);
  });

  it('leaves no query string for the default view', () => {
    // "?" in the URL then means "something is narrowing this", which is also what makes a shared
    // link unambiguous.
    expect(write(DEFAULTS).toString()).toBe('');
    expect(read('')).toEqual(DEFAULTS);
  });

  it('accepts a node deep-link on its own', () => {
    // The reason these filters are in the URL at all: a node page links to its own alert history.
    expect(readScope(new URLSearchParams('node_id=abc'))).toEqual({ nodeId: 'abc', groupId: '' });
    expect(read('node_id=abc')).toEqual(DEFAULTS);
  });

  it('keeps a stale bookmark on the default view rather than on a broken control', () => {
    // ⚠️ The division of labour changed with Inc.10 and this is the pair that shows it: reading no
    // longer drops an unknown token (the control simply has nothing ticked), and `queryFor` is
    // what stops it reaching the API. An unknown *range* still resolves at read time, because
    // `decodeRange` is what the control itself reads through.
    expect(query({ severity: 'fatal', state: 'melted', phase: 'exploded' })).toMatchObject({
      severity: undefined,
      state: undefined,
      resolved: undefined,
    });
    expect(queryFor(COLUMNS, read('range=forever'), NO_SCOPE, null, NOW).since).toBeUndefined();
  });

  it('round-trips a search term whose first character means something to the codec', () => {
    // ⚠️ **The one place Inc.10 changed what the URL looks like.** A text column now writes the
    // encoded condition (`filterCondition.ts`), where it used to write the bare term, so a term
    // starting with `!`, `~` or `\` gains a backslash — `?node_q=%5C%21core` for `!core`. What
    // must hold is that the term survives the round trip unchanged, in the URL and in the request.
    for (const term of ['!core', '~core', '\\core', 'core-sw', '!']) {
      const state = { ...DEFAULTS, node_q: contains(term) };
      const back = read(write(state).toString());
      expect(decodeCondition(back.node_q ?? '').term, term).toBe(term);
      expect(queryFor(COLUMNS, back, NO_SCOPE, null, NOW).node_q, term).toBe(term);
    }
  });

  it('reads a pre-Inc.10 bookmark as a search for the rest of the word', () => {
    // The accepted cost, stated rather than hidden (2026-08-14): a link saved before this change
    // spells `?node_q=!core`, which the shared codec now reads as a *negated* `core`. The column
    // offers no NOT and the request carries only the term, so the row is "core" rather than
    // "!core" — a wider search, on a name no fleet actually uses.
    expect(queryFor(COLUMNS, read('node_q=%21core'), NO_SCOPE, null, NOW).node_q).toBe('core');
  });
});

describe('the filter row (ADR-053 Inc.4)', () => {
  it('keys its columns by the query parameters this screen already used', () => {
    // ⚠️ The column key IS the URL key (ADR-053 decision 12). The columns were `sev` and `at`; they
    // were renamed to `severity` and `range` so that every bookmark taken before the filter row
    // shipped still resolves. A column key is internal — a URL someone saved is not. The Inc.4b
    // columns follow the same rule: `node_q` and `metric` are the API's names, not `node`/`what`.
    expect(reservedKeyCollisions(COLUMNS)).toEqual([]);
  });

  it('offers a multi-select on every enum, because the endpoint takes a set', () => {
    // 🚨 What must stay true is the *pairing*: a multi-select over a parameter that accepts one
    // value drops rows with nothing on screen saying so. `phase` and `acked` are the exception
    // that proves it — they are sets in the control and booleans on the wire, and `queryFor`
    // sends "both ticked" as no parameter at all rather than as one of the two.
    const specs = historyFilters(t);
    for (const key of ['severity', 'state', 'phase', 'acked']) {
      expect(specs[key].kind, key).toBe('enum');
      expect('single' in specs[key], `${key} still carries the removed single flag`).toBe(false);
    }
  });

  it('mounts a filter on every column, so none reads as forgotten', () => {
    // The user-visible half of Inc.4b. Node / What / Acked each had a *reason* to be unfilterable —
    // no API parameter existed — and from the screen that is indistinguishable from an oversight,
    // which is why it was reported twice. If a column here loses its filter again, that is a
    // decision someone has to make deliberately.
    expect(COLUMNS.map((c) => c.key).sort()).toEqual(
      ['acked', 'metric', 'node_q', 'phase', 'range', 'severity', 'state'].sort(),
    );
  });

  it('carries no row accessor at all, because the server answers this list', () => {
    // A `readTime` would re-apply the window in the browser against a different clock reading than
    // the server used; any other accessor would filter one keyset page and hide older matches.
    for (const c of COLUMNS) {
      const spec = c.filter as unknown as Record<string, unknown>;
      for (const accessor of ['readValue', 'readText', 'readTime', 'readNumber', 'readValues']) {
        expect(spec[accessor], `${c.key}.${accessor}`).toBeUndefined();
      }
    }
    expect(DEFAULTS.range).toBe('all');
  });
});
