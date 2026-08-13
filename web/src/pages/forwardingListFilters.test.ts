// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Settings ▸ Forwarding list filter (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { ForwardDestination } from '../types/api';
import { ALL_POOLS, forwardingFilters } from './forwardingListFilters';
import {
  defaultFilters,
  isAnyFiltered,
  reservedKeyCollisions,
  type FilterState,
  type FilterableColumn,
} from '../lib/columnFilter';
import { applyFilters, matchesFilters } from '../lib/filterPredicate';
import { facetCounts } from '../lib/filterCounts';

const dest = (over: Partial<ForwardDestination> = {}): ForwardDestination => ({
  id: 'd1',
  name: 'siem relay',
  source_kind: 'syslog',
  dest_kind: 'syslog_udp',
  target: '10.0.0.9:514',
  enabled: true,
  verbatim: true,
  has_secret: false,
  ca_cert: null,
  filter: { mode: 'all', conditions: [] },
  pool: null,
  rate_limit_per_sec: null,
  ...over,
});

/** A translator stand-in that returns the key, so a missing label shows up as its key. */
const t = ((k: string) => k) as unknown as Parameters<typeof forwardingFilters>[0];

const ROWS = [dest()];
const columnsFor = (rows: readonly ForwardDestination[]): FilterableColumn<ForwardDestination>[] =>
  Object.entries(forwardingFilters(t, rows)).map(([key, filter]) => ({ key, filter }));

const COLUMNS = columnsFor(ROWS);
const DEFAULTS = defaultFilters(COLUMNS);
const f = (over: Record<string, string>): FilterState => ({ ...DEFAULTS, ...over });
const NOW = Date.parse('2026-08-13T12:00:00Z');

describe('the forwarding filter row', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesFilters(dest(), COLUMNS, DEFAULTS, NOW)).toBe(true);
    expect(isAnyFiltered(COLUMNS, DEFAULTS)).toBe(false);
  });

  it('uses column keys that do not collide with the page own query params', () => {
    expect(reservedKeyCollisions(COLUMNS)).toEqual([]);
  });

  it('filters by destination kind, which now has its own column', () => {
    // ⚠️ Before ADR-053 Inc.3 this lived in a toolbar dropdown while the kind was rendered as a
    // sub-line of Target. A filter row needs one fact per column, so the kind got the column.
    expect(matchesFilters(dest(), COLUMNS, f({ dest: 'syslog_udp' }), NOW)).toBe(true);
    expect(matchesFilters(dest(), COLUMNS, f({ dest: 'bigquery' }), NOW)).toBe(false);
    // …and several at once, which the single-choice dropdown could not do.
    const rows = [dest({ id: 'a' }), dest({ id: 'b', dest_kind: 'bigquery' })];
    expect(applyFilters(rows, COLUMNS, f({ dest: 'syslog_udp,bigquery' }), NOW)).toHaveLength(2);
  });

  it('filters by enabled state', () => {
    expect(matchesFilters(dest({ enabled: false }), COLUMNS, f({ status: 'disabled' }), NOW)).toBe(
      true,
    );
    expect(matchesFilters(dest({ enabled: false }), COLUMNS, f({ status: 'enabled' }), NOW)).toBe(
      false,
    );
  });

  it('searches the target and the name as separate columns', () => {
    // The address is what an operator knows during an incident; the name is whatever someone typed
    // months ago. They are two columns now, so each can be asked about on its own.
    expect(matchesFilters(dest(), COLUMNS, f({ target: '10.0.0.9' }), NOW)).toBe(true);
    expect(matchesFilters(dest(), COLUMNS, f({ name: 'SIEM' }), NOW)).toBe(true);
    expect(matchesFilters(dest(), COLUMNS, f({ target: 'SIEM' }), NOW)).toBe(false);
    expect(matchesFilters(dest(), COLUMNS, f({ target: '10.0.0.8' }), NOW)).toBe(false);
  });

  it('makes "pinned to no pool" selectable rather than unfilterable', () => {
    // `null` is the common case and a real answer to "which of these is not pinned to a site", so
    // it needs a token. `''` cannot serve — that is the value meaning *unfiltered*.
    const rows = [dest({ id: 'a', pool: null }), dest({ id: 'b', pool: 'site-b' })];
    const cols = columnsFor(rows);
    expect(applyFilters(rows, cols, { ...defaultFilters(cols), scope: ALL_POOLS }, NOW)).toEqual([
      rows[0],
    ]);
    expect(applyFilters(rows, cols, { ...defaultFilters(cols), scope: 'site-b' }, NOW)).toEqual([
      rows[1],
    ]);
  });

  it('discovers pool options from the rows, sorted and deduplicated', () => {
    const rows = [
      dest({ id: 'a', pool: 'site-b' }),
      dest({ id: 'b', pool: 'site-a' }),
      dest({ id: 'c', pool: 'site-b' }),
    ];
    const scope = forwardingFilters(t, rows).scope;
    expect(scope.kind === 'enum' && scope.options.map((o) => o.value)).toEqual([
      ALL_POOLS,
      'site-a',
      'site-b',
    ]);
  });

  it('counts a facet over the rows that pass the OTHER filters', () => {
    const rows = [
      dest({ id: 'a', dest_kind: 'syslog_udp', enabled: true }),
      dest({ id: 'b', dest_kind: 'bigquery', enabled: false }),
      dest({ id: 'c', dest_kind: 'bigquery', enabled: true }),
    ];
    const cols = columnsFor(rows);
    const state = { ...defaultFilters(cols), dest: 'syslog_udp' };
    // Its own filter is excluded, so `bigquery` still reports what switching would give.
    const counts = facetCounts(rows, cols, state, 'dest', NOW);
    expect(counts.syslog_udp).toBe(1);
    expect(counts.bigquery).toBe(2);
  });

  it('flips isAnyFiltered for every column', () => {
    for (const key of Object.keys(DEFAULTS)) {
      expect(isAnyFiltered(COLUMNS, f({ [key]: 'x' }))).toBe(true);
    }
  });
});
