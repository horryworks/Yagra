// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Topology ▸ Dependencies filter row (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { TopologyNode } from '../types/api';
import { dependencyFilters, REF_NONE, REF_SET } from './dependencyFilters';
import type { DiffRow } from './topologyDiff';
import {
  defaultFilters,
  isAnyFiltered,
  reservedKeyCollisions,
  type FilterState,
  type FilterableColumn,
} from '../lib/columnFilter';
import { applyFilters, matchesFilters } from '../lib/filterPredicate';

const t = ((k: string) => k) as unknown as Parameters<typeof dependencyFilters>[0];
const NOW = Date.parse('2026-08-13T12:00:00Z');

const node = (over: Partial<TopologyNode> = {}): TopologyNode =>
  ({ id: 'n1', name: 'core-sw-1', parent_id: null, root_cause: null, state: 'ok', ...over }) as TopologyNode;

const columnsFor = (
  diffs: ReadonlyMap<string, DiffRow>,
  comparing: boolean,
): FilterableColumn<TopologyNode>[] =>
  Object.entries(dependencyFilters(t, diffs, comparing)).map(([key, filter]) => ({ key, filter }));

const COLUMNS = columnsFor(new Map(), false);
const DEFAULTS = defaultFilters(COLUMNS);
const f = (over: Record<string, string>): FilterState => ({ ...DEFAULTS, ...over });

describe('the dependency filter row', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesFilters(node(), COLUMNS, DEFAULTS, NOW)).toBe(true);
    expect(isAnyFiltered(COLUMNS, DEFAULTS)).toBe(false);
    expect(reservedKeyCollisions(COLUMNS)).toEqual([]);
  });

  it('splits the old three-way select across the two columns it was really about', () => {
    // ⚠️ The toolbar offered `all` / `With upstream` / `Currently suppressed` — one control asking
    // two different columns' questions, so the two could never be combined. They can now.
    const rows = [
      node({ id: 'a' }),
      node({ id: 'b', parent_id: 'a' }),
      node({ id: 'c', parent_id: 'a', root_cause: 'a' }),
    ];
    expect(applyFilters(rows, COLUMNS, f({ upstream: REF_SET }), NOW).map((r) => r.id)).toEqual([
      'b',
      'c',
    ]);
    expect(applyFilters(rows, COLUMNS, f({ root: REF_SET }), NOW).map((r) => r.id)).toEqual(['c']);
    // Both at once — the combination the single select could not express.
    expect(
      applyFilters(rows, COLUMNS, f({ upstream: REF_SET, root: REF_NONE }), NOW).map((r) => r.id),
    ).toEqual(['b']);
  });

  it('offers node state, which the toolbar had no control for at all', () => {
    const rows = [node({ id: 'a', state: 'ok' }), node({ id: 'b', state: 'critical' })];
    expect(applyFilters(rows, COLUMNS, f({ status: 'critical' }), NOW).map((r) => r.id)).toEqual([
      'b',
    ]);
    // Worst first, so what an operator came here for is at the top of the list.
    const status = dependencyFilters(t, new Map(), false).status;
    expect(status.kind === 'enum' && status.options[0].value).toBe('critical');
  });

  it('only offers the verdict column while a derived graph is being compared', () => {
    // A spec for a column that is not rendered is a filter reachable from a URL and invisible on
    // screen — the operator would see a narrowed list with nothing saying why.
    expect(Object.keys(dependencyFilters(t, new Map(), false))).not.toContain('verdict');
    expect(Object.keys(dependencyFilters(t, new Map(), true))).toContain('verdict');
  });

  it('filters on the verdict the cell actually renders', () => {
    const diffs = new Map<string, DiffRow>([
      ['a', { nodeId: 'a', verdict: 'agree', manualOnly: [], derivedOnly: [] }],
      ['b', { nodeId: 'b', verdict: 'only_derived', manualOnly: [], derivedOnly: [] }],
    ]);
    const cols = columnsFor(diffs, true);
    const rows = [node({ id: 'a' }), node({ id: 'b' }), node({ id: 'c' })];
    const state = { ...defaultFilters(cols), verdict: 'only_derived' };
    expect(applyFilters(rows, cols, state, NOW).map((r) => r.id)).toEqual(['b']);
    // `c` has no diff row and the cell shows an em dash; a selection excludes it, which is what
    // "no verdict" has to mean once the operator has picked one.
    const agree = { ...defaultFilters(cols), verdict: 'agree' };
    expect(applyFilters(rows, cols, agree, NOW).map((r) => r.id)).toEqual(['a']);
  });

  it('searches node names, and can exclude', () => {
    const rows = [node({ id: 'a', name: 'core-sw-1' }), node({ id: 'b', name: 'edge-rtr-2' })];
    expect(applyFilters(rows, COLUMNS, f({ node: 'CORE' }), NOW).map((r) => r.id)).toEqual(['a']);
    expect(applyFilters(rows, COLUMNS, f({ node: '!core' }), NOW).map((r) => r.id)).toEqual(['b']);
  });
});
