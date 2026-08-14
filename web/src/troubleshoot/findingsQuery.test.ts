// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import { defaultFilters, isAnyFiltered, specColumns } from '../lib/columnFilter';
import { encodeCondition } from '../lib/filterCondition';
import {
  PAGE_SIZE,
  appendPage,
  findingFilters,
  nextCursor,
  queryFor,
  scopeFilter,
  scopeIsSet,
  type ScopeIds,
} from './findingsQuery';
import type { SavedFinding } from '../types/api';

const NOW = Date.parse('2026-08-02T12:00:00.000Z');
const t = ((k: string) => k) as unknown as TFunction;
const COLUMNS = specColumns(findingFilters(t));
/** The state the screen opens with — derived from the specs, exactly as the page derives it. */
const DEFAULTS = defaultFilters(COLUMNS);
const NO_SCOPE: ScopeIds = { nodeId: '', groupId: '' };
const query = (
  over: Record<string, string> = {},
  scope: ScopeIds = NO_SCOPE,
  cursor: { before: string; before_id: string } | null = null,
) => queryFor(COLUMNS, { ...DEFAULTS, ...over }, scope, cursor, NOW);
const contains = (term: string) => encodeCondition({ term, mode: 'contains', not: false });

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

describe('the default window', () => {
  it('is a week, and comes off the spec rather than a defaults object', () => {
    // 7d, not `all`: an unbounded default gets slower as the table fills, and the first screen an
    // operator opens is the wrong place to discover that. Since Inc.10 the spec's `defaultPreset`
    // is the only place it is written, so `defaultFilters` cannot disagree with the control.
    expect(DEFAULTS.range).toBe('7d');
    expect(query().since).toBe('2026-07-26T12:00:00.000Z');
  });

  it('turns each preset into an absolute bound, and "all" into none', () => {
    expect(query({ range: '24h' }).since).toBe('2026-08-01T12:00:00.000Z');
    expect(query({ range: '30d' }).since).toBe('2026-07-03T12:00:00.000Z');
    expect(query({ range: 'all' }).since).toBeUndefined();
  });

  it('falls back to the default window for a preset a stale URL names', () => {
    // Not to "all time" — the widening answer is the dangerous one, so the length is looked up in
    // the spec's own presets after `decodeRange` has already replaced anything unknown.
    expect(query({ range: 'last-fortnight' }).since).toBe(query().since);
  });
});

describe('queryFor', () => {
  it('omits every unset filter rather than sending an empty string', () => {
    // The failure this pins: `severity: ''` reaches the backend as `?severity=` and is rejected as
    // an unknown severity — a filter nobody set turning the screen into a 400.
    const q = query({ range: 'all' });
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
    expect(
      query(
        {
          // Several tools in one request — the joined spelling is what the API takes. Note the
          // order flips below: `normalizeSets` re-encodes the set in the tool catalog's order, so
          // the same selection is the same string however it was clicked.
          tool: 'flap,capacity',
          severity: 'warn',
          q: contains('cpu'),
          node_q: contains('core-sw'),
          score: '60:',
          range: '24h',
        },
        { nodeId: 'node-7', groupId: 'group-3' },
        { before: '2026-08-02T01:00:00Z', before_id: 'f9' },
      ),
    ).toEqual({
      tool: 'capacity,flap',
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

  it('drops a token the column does not offer instead of forwarding it', () => {
    // 🚨 The trap decision AA had to close: the state is not validated on the way in (a stale
    // bookmark must open the default view), so `?severity=bogus` would otherwise arrive at an
    // endpoint that rejects unknown severities.
    expect(query({ severity: 'bogus' }).severity).toBeUndefined();
    expect(query({ severity: 'bogus,crit' }).severity).toBe('crit');
    expect(query({ tool: 'not-a-tool' }).tool).toBeUndefined();
  });

  it('sends a score bound of zero rather than dropping it', () => {
    // `f.score || undefined` would be right for every other filter on this screen and wrong for
    // this one: 0 is a real floor on a 0–100 score, and dropping it widens the search silently.
    const q = query({ score: '0:20' });
    expect(q.min_score).toBe(0);
    expect(q.max_score).toBe(20);
  });

  it('omits both bounds when the score column is unset', () => {
    expect(query().min_score).toBeUndefined();
    expect(query().max_score).toBeUndefined();
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
    expect(scopeFilter({ kind: 'all', id: null, label: '' })).toEqual(NO_SCOPE);
  });
});

describe('the empty state discriminator', () => {
  it('asks the columns and the scope, because either can hide rows', () => {
    // A "clear all" that leaves a node selected is a lie, and so is an empty state that says the
    // store is empty while the scope is narrowing it. The columns half is derived from the specs,
    // so a filter added without a clause cannot slip past it.
    expect(isAnyFiltered(COLUMNS, DEFAULTS) || scopeIsSet(NO_SCOPE)).toBe(false);
    expect(isAnyFiltered(COLUMNS, { ...DEFAULTS, severity: 'crit' })).toBe(true);
    expect(isAnyFiltered(COLUMNS, { ...DEFAULTS, range: 'all' })).toBe(true);
    expect(scopeIsSet({ nodeId: 'n1', groupId: '' })).toBe(true);
    expect(scopeIsSet({ nodeId: '', groupId: 'g1' })).toBe(true);
  });
});
