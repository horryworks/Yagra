// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Settings ▸ Pollers filter row (no DOM — Vitest node env).
//
// `matchesPoller` is gone (ADR-053 Inc.5): the row test is now the shared
// `lib/filterPredicate.ts::buildPredicate`, and what is left to test is what each column reads.

import { describe, expect, it } from 'vitest';
import type { PollerInfo } from '../types/api';
import { pollerFilters, POLLER_STATUSES } from './pollerFilters';
import { specColumns, type ColumnFilterSpec, type FilterState } from '../lib/columnFilter';
import { buildPredicate } from '../lib/filterPredicate';
import { encodeCondition } from '../lib/filterCondition';

const poller = (over: Partial<PollerInfo> = {}): PollerInfo => ({
  id: 'tokyo-1',
  pool: 'tokyo',
  status: 'online',
  version: '0.2.5',
  caps: [],
  listeners: [],
  mgmt_addrs: [],
  first_seen: '2026-01-01T00:00:00.000Z',
  last_seen: '2026-08-13T00:00:00.000Z',
  working_set_nodes: 100,
  working_set_specs: 200,
  results_total: 1000,
  cpu_pct: null,
  mem_used_pct: null,
  disk_used_pct: null,
  anchor_node_id: null,
  can_change_pool: true,
  has_token: false,
  token_issued_at: null,
  ...over,
});

const t = ((k: string) => k) as unknown as Parameters<typeof pollerFilters>[0];
const specs = pollerFilters(t, ['tokyo', 'osaka']);
const term = (s: string) => encodeCondition({ term: s, mode: 'contains', not: false });

function keeps(row: PollerInfo, state: FilterState): boolean {
  return buildPredicate(
    specColumns(specs as Record<string, ColumnFilterSpec<PollerInfo>>),
    state,
    0,
  )(row);
}

describe('the pollers filter row', () => {
  it('shows everything when nothing is set', () => {
    expect(keeps(poller(), {})).toBe(true);
  });

  it('offers exactly the two states a poller reports', () => {
    expect(POLLER_STATUSES).toEqual(['online', 'offline']);
  });

  it('filters by status and by pool independently', () => {
    expect(keeps(poller(), { status: 'online' })).toBe(true);
    expect(keeps(poller(), { status: 'offline' })).toBe(false);
    expect(keeps(poller(), { pool: 'tokyo' })).toBe(true);
    expect(keeps(poller(), { pool: 'osaka' })).toBe(false);
  });

  it('offers every pool the deployment has, not only the ones with a live poller', () => {
    // A pool whose only poller died still exists, and "show me the pollers in osaka" deserves an
    // honest empty answer rather than a missing option.
    const pool = specs.pool;
    expect(pool.kind === 'enum' && pool.options.map((o) => o.value)).toEqual(['tokyo', 'osaka']);
  });

  it('filters the version under its own column, which is the rollout question', () => {
    // "Which boxes are still on the old build" is asked during every upgrade (ADR-051), and the
    // column is otherwise only readable by eye.
    expect(keeps(poller(), { version: term('0.2.5') })).toBe(true);
    expect(keeps(poller(), { version: term('0.2.4') })).toBe(false);
  });

  it('separates the id from the version, which one search box could not', () => {
    // The old box read id + pool + version at once, so `0.2` matched a poller on v0.2.4 and a
    // poller in a pool named `site-0.2` identically.
    expect(keeps(poller(), { poller: term('TOKYO-1') })).toBe(true);
    expect(keeps(poller(), { poller: term('0.2.5') })).toBe(false);
    expect(keeps(poller({ id: 'site-0.2-a' }), { version: term('0.2') })).toBe(true);
  });

  it('matches nothing on the em dash a poller with no version renders', () => {
    // The cell shows `—`; the filter must read the empty string, or typing a dash would select
    // exactly the rows that have no version — matching on punctuation nobody entered.
    expect(keeps(poller({ version: null }), { version: term('—') })).toBe(false);
    expect(keeps(poller({ version: null }), { version: term('0.2') })).toBe(false);
  });
});
