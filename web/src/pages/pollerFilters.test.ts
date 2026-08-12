// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Settings ▸ Pollers filter (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { PollerInfo } from '../types/api';
import {
  DEFAULT_POLLER_FILTERS,
  isPollerFiltered,
  matchesPoller,
  POLLER_STATUSES,
  type PollerFilters,
} from './pollerFilters';

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
  ...over,
});

const f = (over: Partial<PollerFilters>): PollerFilters => ({ ...DEFAULT_POLLER_FILTERS, ...over });

describe('matchesPoller', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesPoller(poller(), DEFAULT_POLLER_FILTERS)).toBe(true);
  });

  it('offers exactly the two states a poller reports', () => {
    expect(POLLER_STATUSES).toEqual(['online', 'offline']);
  });

  it('filters by status and by pool independently', () => {
    expect(matchesPoller(poller(), f({ status: 'online' }))).toBe(true);
    expect(matchesPoller(poller(), f({ status: 'offline' }))).toBe(false);
    expect(matchesPoller(poller(), f({ pool: 'tokyo' }))).toBe(true);
    expect(matchesPoller(poller(), f({ pool: 'osaka' }))).toBe(false);
  });

  it('searches the version, which is the rollout question', () => {
    // "Which boxes are still on the old build" is asked during every upgrade, and the column is
    // otherwise only readable by eye.
    expect(matchesPoller(poller(), f({ q: '0.2.5' }))).toBe(true);
    expect(matchesPoller(poller(), f({ q: '0.2.4' }))).toBe(false);
  });

  it('searches the id and the pool', () => {
    expect(matchesPoller(poller(), f({ q: 'TOKYO-1' }))).toBe(true);
    expect(matchesPoller(poller(), f({ q: 'tokyo' }))).toBe(true);
    expect(matchesPoller(poller(), f({ q: 'osaka' }))).toBe(false);
  });

  it('flips isFiltered for every field', () => {
    expect(isPollerFiltered(DEFAULT_POLLER_FILTERS)).toBe(false);
    for (const x of [f({ status: 'offline' }), f({ pool: 'p' }), f({ q: 'x' })]) {
      expect(isPollerFiltered(x)).toBe(true);
    }
  });
});
