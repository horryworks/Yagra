// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the alert-history filter state (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import { SEVERITIES, type NodeState, type Severity } from '../types/api';
import { SEVERITY_ORDER } from '../lib/nodeState';
import { reservedKeyCollisions } from '../lib/columnFilter';
import {
  DEFAULT_FILTERS,
  filtersFromState,
  historyFilters,
  isFiltered,
  stateFromFilters,
  queryFor,
  readFilters,
  resolvedFor,
  writeFilters,
  type HistoryFilters,
} from './historyQuery';
import { PAGE_SIZE } from './historyCursor';

const NOW = Date.parse('2026-08-12T00:00:00.000Z');
const read = (qs: string) =>
  readFilters(new URLSearchParams(qs), SEVERITIES, SEVERITY_ORDER as readonly NodeState[]);

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
    expect(queryFor(DEFAULT_FILTERS, null, NOW)).toEqual({
      limit: PAGE_SIZE,
      severity: undefined,
      state: undefined,
      resolved: undefined,
      node_id: undefined,
      group_id: undefined,
      since: undefined,
      before: undefined,
      before_id: undefined,
    });
  });

  it('maps each control to its own parameter', () => {
    const f: HistoryFilters = {
      severity: 'critical' as Severity,
      state: 'unreachable' as NodeState,
      phase: 'fired',
      range: '7d',
      nodeId: 'n1',
      groupId: '',
    };
    expect(queryFor(f, null, NOW)).toMatchObject({
      severity: 'critical',
      state: 'unreachable',
      resolved: false,
      node_id: 'n1',
      group_id: undefined,
      since: '2026-08-05T00:00:00.000Z',
    });
  });

  it('keeps the window while the cursor advances', () => {
    // `since` is what the operator asked to see; the cursor is where this page starts. Conflating
    // them would reset the window on every scroll.
    const f: HistoryFilters = { ...DEFAULT_FILTERS, range: '24h' };
    const page2 = queryFor(f, { before: 'ts', before_id: 'id' }, NOW);
    expect(page2.since).toBe('2026-08-11T00:00:00.000Z');
    expect(page2.before).toBe('ts');
    expect(page2.before_id).toBe('id');
  });
});

describe('isFiltered', () => {
  it('is false for the default view', () => {
    expect(isFiltered(DEFAULT_FILTERS)).toBe(false);
  });

  it('flips for every field, including ones added later', () => {
    // Derived from DEFAULT_FILTERS rather than a hand-written disjunction, so a filter added
    // without its clause cannot leave the empty state claiming the log is empty while a filter is
    // hiding rows. Nothing here needs updating to keep that true.
    const changed: Record<keyof HistoryFilters, HistoryFilters> = {
      severity: { ...DEFAULT_FILTERS, severity: 'info' as Severity },
      state: { ...DEFAULT_FILTERS, state: 'ok' as NodeState },
      phase: { ...DEFAULT_FILTERS, phase: 'fired' },
      range: { ...DEFAULT_FILTERS, range: '24h' },
      nodeId: { ...DEFAULT_FILTERS, nodeId: 'n1' },
      groupId: { ...DEFAULT_FILTERS, groupId: 'g1' },
    };
    for (const key of Object.keys(DEFAULT_FILTERS) as (keyof HistoryFilters)[]) {
      expect(isFiltered(changed[key]), `${key} did not register as a filter`).toBe(true);
    }
  });
});

describe('URL round trip', () => {
  it('restores every filter from the query string', () => {
    const f: HistoryFilters = {
      severity: 'warning' as Severity,
      state: 'critical' as NodeState,
      phase: 'cleared',
      range: '30d',
      nodeId: 'n1',
      groupId: 'g1',
    };
    const p = new URLSearchParams();
    writeFilters(p, f);
    expect(read(p.toString())).toEqual(f);
  });

  it('leaves no query string for the default view', () => {
    // "?" in the URL then means "something is narrowing this", which is also what makes a shared
    // link unambiguous.
    const p = new URLSearchParams();
    writeFilters(p, DEFAULT_FILTERS);
    expect(p.toString()).toBe('');
    expect(read('')).toEqual(DEFAULT_FILTERS);
  });

  it('falls back rather than surfacing a value no control can render', () => {
    // A stale bookmark from an older release, or a hand-edited URL. The screen recovers by showing
    // the default view — unlike the API edge, where an unknown token is a 400 so a typo cannot
    // silently widen a search.
    expect(read('severity=fatal&state=melted&phase=exploded&range=forever')).toEqual(
      DEFAULT_FILTERS,
    );
  });

  it('accepts a node deep-link on its own', () => {
    // The reason these filters are in the URL at all: a node page links to its own alert history.
    expect(read('node_id=abc')).toEqual({ ...DEFAULT_FILTERS, nodeId: 'abc' });
  });
});

describe('the filter row (ADR-053 Inc.4)', () => {
  const t = ((k: string) => k) as unknown as Parameters<typeof historyFilters>[0];
  const specs = historyFilters(t);
  const COLUMNS = Object.entries(specs).map(([key, filter]) => ({ key, filter }));

  it('keys its columns by the query parameters this screen already used', () => {
    // ⚠️ The column key IS the URL key (ADR-053 decision 12). The columns were `sev` and `at`; they
    // were renamed to `severity` and `range` so that every bookmark taken before the filter row
    // shipped still resolves. A column key is internal — a URL someone saved is not.
    expect(COLUMNS.map((c) => c.key).sort()).toEqual(['phase', 'range', 'severity', 'state']);
    expect(reservedKeyCollisions(COLUMNS)).toEqual([]);
  });

  it('offers one value per enum, because the endpoint takes one', () => {
    // 🚨 A multi-select here would let an operator tick three severities and send one — rows missing
    // from a filtered list with nothing on screen saying so. `GET /alerts/history` takes a single
    // `severity`/`state`/`resolved`; widening it is a backend increment with its own MCP parity.
    for (const key of ['severity', 'state', 'phase']) {
      const s = specs[key];
      expect(s.kind === 'enum' && s.single).toBe(true);
    }
  });

  it('carries no client-side range accessor, so the window is applied once', () => {
    // The window becomes `since` in the request. A `readTime` here would re-apply it in the browser
    // against a different clock reading than the server used, dropping rows at the boundary.
    const range = specs.range;
    expect(range.kind === 'range' && range.readTime).toBeUndefined();
    expect(range.kind === 'range' && range.defaultPreset).toBe(DEFAULT_FILTERS.range);
  });

  it('round-trips every filter through the flat row state', () => {
    const f: HistoryFilters = {
      severity: 'critical' as Severity,
      state: 'unreachable' as NodeState,
      phase: 'cleared',
      range: '7d',
      nodeId: 'n1',
      groupId: 'g1',
    };
    // The scope is not a column, so it is supplied on the way back rather than read off the row.
    expect(filtersFromState(stateFromFilters(f), { nodeId: 'n1', groupId: 'g1' })).toEqual(f);
  });

  it('falls back to the default window for a preset a stale URL names', () => {
    const back = filtersFromState({ ...stateFromFilters(DEFAULT_FILTERS), range: 'last-fortnight' }, {
      nodeId: '',
      groupId: '',
    });
    expect(back.range).toBe(DEFAULT_FILTERS.range);
  });
});
