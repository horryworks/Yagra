// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Troubleshoot ▸ Scheduled filter row (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { AnalysisSchedule } from '../types/api';
import { scheduleFilters } from './scheduleFilters';
import { TOOLS } from './data';
import {
  defaultFilters,
  isAnyFiltered,
  reservedKeyCollisions,
  type FilterState,
  type FilterableColumn,
} from '../lib/columnFilter';
import { applyFilters, matchesFilters } from '../lib/filterPredicate';

const t = ((k: string) => k) as unknown as Parameters<typeof scheduleFilters>[0];
const NOW = Date.parse('2026-08-13T12:00:00Z');

const sched = (over: Partial<AnalysisSchedule> = {}): AnalysisSchedule =>
  ({
    id: 's1',
    tool: TOOLS[0].id,
    scope_label: 'core switches',
    scope_id: null,
    enabled: true,
    next_run_ms: Date.parse('2026-08-14T00:00:00Z'),
    last_status: 'queued',
    ...over,
  }) as AnalysisSchedule;

const COLUMNS: FilterableColumn<AnalysisSchedule>[] = Object.entries(scheduleFilters(t)).map(
  ([key, filter]) => ({ key, filter }),
);
const DEFAULTS = defaultFilters(COLUMNS);
const f = (over: Record<string, string>): FilterState => ({ ...DEFAULTS, ...over });

describe('the scheduled-analysis filter row', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesFilters(sched(), COLUMNS, DEFAULTS, NOW)).toBe(true);
    expect(isAnyFiltered(COLUMNS, DEFAULTS)).toBe(false);
    expect(reservedKeyCollisions(COLUMNS)).toEqual([]);
  });

  it('takes its analysis options from the catalog, not a second list', () => {
    // A new analysis has to appear here without anyone remembering to add it — the catalog is what
    // the schedule form already offers.
    const tool = scheduleFilters(t).tool;
    expect(tool.kind === 'enum' && tool.options.map((o) => o.value)).toEqual(TOOLS.map((x) => x.id));
  });

  it('filters by analysis, several at once', () => {
    const rows = [sched({ id: 'a' }), sched({ id: 'b', tool: TOOLS[1].id })];
    expect(applyFilters(rows, COLUMNS, f({ tool: TOOLS[1].id }), NOW).map((r) => r.id)).toEqual([
      'b',
    ]);
    expect(
      applyFilters(rows, COLUMNS, f({ tool: `${TOOLS[0].id},${TOOLS[1].id}` }), NOW),
    ).toHaveLength(2);
  });

  it('puts the paused/running choice on the column that actually shows it', () => {
    // The Next-run cell reads "Paused" for a disabled schedule instead of a time, so that column is
    // where an operator looks for the distinction — not a toolbar dropdown three controls away.
    const rows = [sched({ id: 'a' }), sched({ id: 'b', enabled: false })];
    expect(applyFilters(rows, COLUMNS, f({ next: 'disabled' }), NOW).map((r) => r.id)).toEqual(['b']);
    expect(applyFilters(rows, COLUMNS, f({ next: 'enabled' }), NOW).map((r) => r.id)).toEqual(['a']);
  });

  it('filters on the firing outcome, and excludes one that has never run', () => {
    const rows = [sched({ id: 'a', last_status: 'error' }), sched({ id: 'b', last_status: null })];
    expect(applyFilters(rows, COLUMNS, f({ last: 'error' }), NOW).map((r) => r.id)).toEqual(['a']);
    // `b` renders an em dash; once a status is selected it is not one of them.
    expect(applyFilters(rows, COLUMNS, f({ last: 'queued' }), NOW)).toEqual([]);
  });

  it('searches the scope label, which is what the row shows', () => {
    // Not `scope_id` — that is a UUID nobody types.
    expect(matchesFilters(sched(), COLUMNS, f({ scope: 'CORE' }), NOW)).toBe(true);
    expect(matchesFilters(sched(), COLUMNS, f({ scope: 'edge' }), NOW)).toBe(false);
    expect(matchesFilters(sched(), COLUMNS, f({ scope: '!edge' }), NOW)).toBe(true);
  });

  it('flips isAnyFiltered for every column', () => {
    for (const key of Object.keys(DEFAULTS)) {
      expect(isAnyFiltered(COLUMNS, f({ [key]: 'x' }))).toBe(true);
    }
  });
});
