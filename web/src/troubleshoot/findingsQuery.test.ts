// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  DEFAULT_FILTERS,
  PAGE_SIZE,
  appendPage,
  isFiltered,
  nextCursor,
  queryFor,
  scopeFilter,
  sinceIso,
  type FindingFilters,
} from './findingsQuery';
import type { SavedFinding } from '../types/api';

const NOW = Date.parse('2026-08-02T12:00:00.000Z');

function row(over: Partial<SavedFinding> = {}): SavedFinding {
  return {
    id: 'f1',
    job_id: 'j1',
    tool: 'anomaly',
    score: 91,
    severity: 'crit',
    node_id: 'n1',
    node_name: 'core-sw-01',
    metric: 'cpu',
    kind: 'spike',
    when_label: 'today 03:12',
    duration: '6 min',
    at: '2026-08-02T03:12:00.000Z',
    ...over,
  };
}

describe('sinceIso', () => {
  it('turns each preset into an absolute bound, and "all" into none', () => {
    expect(sinceIso('24h', NOW)).toBe('2026-08-01T12:00:00.000Z');
    expect(sinceIso('7d', NOW)).toBe('2026-07-26T12:00:00.000Z');
    expect(sinceIso('30d', NOW)).toBe('2026-07-03T12:00:00.000Z');
    expect(sinceIso('all', NOW)).toBeUndefined();
  });
});

describe('queryFor', () => {
  it('omits every unset filter rather than sending an empty string', () => {
    // The failure this pins: `severity: ''` reaches the backend as `?severity=` and is rejected as
    // an unknown severity — a filter nobody set turning the screen into a 400.
    const q = queryFor({ ...DEFAULT_FILTERS, range: 'all' }, null, NOW);
    expect(q.tool).toBeUndefined();
    expect(q.severity).toBeUndefined();
    expect(q.q).toBeUndefined();
    expect(q.node_id).toBeUndefined();
    expect(q.node_q).toBeUndefined();
    expect(q.group_id).toBeUndefined();
    expect(q.since).toBeUndefined();
    expect(q.before).toBeUndefined();
    expect(q.before_id).toBeUndefined();
    expect(q.limit).toBe(PAGE_SIZE);
  });

  it('carries every set filter through', () => {
    const f: FindingFilters = {
      // Several tools in one request — the joined spelling is what the API takes, so `queryFor`
      // passes it straight through rather than re-encoding a list at the last moment.
      tool: 'flap,capacity',
      severity: 'warn',
      q: 'cpu',
      nodeQ: 'core-sw',
      score: '60:',
      range: '24h',
      nodeId: 'node-7',
      groupId: 'group-3',
    };
    expect(queryFor(f, { before: '2026-08-02T01:00:00Z', before_id: 'f9' }, NOW)).toEqual({
      tool: 'flap,capacity',
      severity: 'warn',
      q: 'cpu',
      node_id: 'node-7',
      node_q: 'core-sw',
      group_id: 'group-3',
      min_score: 60,
      max_score: undefined,
      since: '2026-08-01T12:00:00.000Z',
      before: '2026-08-02T01:00:00Z',
      before_id: 'f9',
      limit: PAGE_SIZE,
    });
  });

  it('sends a score bound of zero rather than dropping it', () => {
    // `f.score || undefined` would be right for every other filter on this screen and wrong for
    // this one: 0 is a real floor on a 0–100 score, and dropping it widens the search silently.
    const q = queryFor({ ...DEFAULT_FILTERS, score: '0:20' }, null, NOW);
    expect(q.min_score).toBe(0);
    expect(q.max_score).toBe(20);
  });

  it('omits both bounds when the score column is unset', () => {
    const q = queryFor(DEFAULT_FILTERS, null, NOW);
    expect(q.min_score).toBeUndefined();
    expect(q.max_score).toBeUndefined();
  });
});

describe('nextCursor', () => {
  it('sends both halves of the cursor, from the same row', () => {
    // `before` alone would re-request every finding sharing that millisecond — a run writes its
    // findings in a tight loop, so the page would appear to repeat itself.
    const rows = Array.from({ length: PAGE_SIZE }, (_, i) =>
      row({ id: `f${i}`, at: `2026-08-02T03:12:00.00${i % 10}Z` }),
    );
    expect(nextCursor(rows)).toEqual({
      before: rows[PAGE_SIZE - 1].at,
      before_id: rows[PAGE_SIZE - 1].id,
    });
  });

  it('stops at a short page', () => {
    expect(nextCursor([row()])).toBeNull();
    expect(nextCursor([])).toBeNull();
  });
});

describe('appendPage', () => {
  it('keeps order and drops a row already held', () => {
    const have = [row({ id: 'a' }), row({ id: 'b' })];
    const next = appendPage(have, [row({ id: 'b' }), row({ id: 'c' })]);
    expect(next.map((f) => f.id)).toEqual(['a', 'b', 'c']);
  });
});

describe('scopeFilter', () => {
  it('keeps a node and a group scope apart', () => {
    // They mean different things to the backend — a group covers its whole subtree, a node is
    // exactly one — so a single "id" field would lose which was meant.
    expect(scopeFilter({ kind: 'node', id: 'n1', label: '' })).toEqual({
      nodeId: 'n1',
      groupId: '',
    });
    expect(scopeFilter({ kind: 'group', id: 'g1', label: '' })).toEqual({
      nodeId: '',
      groupId: 'g1',
    });
    expect(scopeFilter({ kind: 'all', id: null, label: '' })).toEqual({ nodeId: '', groupId: '' });
  });
});

describe('isFiltered', () => {
  it('is false only for the default view', () => {
    expect(isFiltered(DEFAULT_FILTERS)).toBe(false);
    expect(isFiltered({ ...DEFAULT_FILTERS, range: 'all' })).toBe(true);
    expect(isFiltered({ ...DEFAULT_FILTERS, severity: 'crit' })).toBe(true);
    expect(isFiltered({ ...DEFAULT_FILTERS, tool: 'capacity' })).toBe(true);
    expect(isFiltered({ ...DEFAULT_FILTERS, nodeId: 'n1' })).toBe(true);
    expect(isFiltered({ ...DEFAULT_FILTERS, groupId: 'g1' })).toBe(true);
  });
});
