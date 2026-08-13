// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the two suppression screens' filter rows (no DOM — Vitest node env).
//
// These used to test `matchesWindow` / `matchesMute`, two hand-written row predicates. ADR-053
// Inc.5 replaced both with column specs run through the shared `buildPredicate`, so what is tested
// here is what each column reads off a row — the only per-screen judgement left. Every property the
// old tests pinned still has a test below; three of them (the boundary, the null metric, the
// lazy name resolution) are the ones that were actually load-bearing.

import { describe, expect, it, vi } from 'vitest';
import type { MaintenanceWindow, Mute } from '../types/api';
import { muteFilters, muteIsExpired, MUTE_STATES, windowFilters } from './suppressionFilters';
import { MAINTENANCE_STATUSES } from './maintenanceStatus';
import { specColumns, type ColumnFilterSpec, type FilterState } from '../lib/columnFilter';
import { buildPredicate } from '../lib/filterPredicate';
import { encodeCondition } from '../lib/filterCondition';

const NOW = Date.parse('2026-08-13T12:00:00.000Z');
const iso = (offsetMs: number) => new Date(NOW + offsetMs).toISOString();
const HOUR = 3_600_000;

const win = (over: Partial<MaintenanceWindow> = {}): MaintenanceWindow => ({
  id: 'w1',
  name: 'core switch upgrade',
  scope_level: 'node',
  scope_id: 'n1',
  starts_at: iso(-HOUR),
  ends_at: iso(HOUR),
  enabled: true,
  active: true,
  ...over,
});

const mute = (over: Partial<Mute> = {}): Mute => ({
  id: 'm1',
  scope_kind: 'node',
  node_id: 'n1',
  group_id: null,
  metric_name: 'icmp_rtt_ms',
  reason: 'noisy link',
  until_at: iso(HOUR),
  ...over,
});

const label = () => 'rtr-01';
const t = ((k: string) => k) as unknown as Parameters<typeof muteFilters>[0];

/** A contains-condition, encoded the way a filter cell stores one. */
const term = (s: string) => encodeCondition({ term: s, mode: 'contains', not: false });

/** Run one row through a screen's specs, exactly as the table does. */
function keeps<T>(
  specs: Record<string, ColumnFilterSpec<T>>,
  row: T,
  state: FilterState,
): boolean {
  return buildPredicate(specColumns(specs), state, NOW)(row);
}

describe('the maintenance filter row', () => {
  const specs = windowFilters(t, label, NOW);

  it('offers the badge column\'s own status vocabulary, not a second list', () => {
    // A status the badge can render but the filter cannot select would be a row an operator can
    // see and not filter for.
    const status = specs.status;
    expect(status.kind).toBe('enum');
    expect(status.kind === 'enum' && status.options.map((o) => o.value)).toEqual([
      ...MAINTENANCE_STATUSES,
    ]);
  });

  it('shows everything when nothing is set', () => {
    expect(keeps(specs, win(), {})).toBe(true);
  });

  it('filters on the same status the badge shows', () => {
    const running = win();
    const ended = win({ active: false, ends_at: iso(-HOUR) });
    const off = win({ enabled: false, active: false, ends_at: iso(HOUR) });
    expect(keeps(specs, running, { status: 'active' })).toBe(true);
    expect(keeps(specs, running, { status: 'ended' })).toBe(false);
    expect(keeps(specs, ended, { status: 'ended' })).toBe(true);
    expect(keeps(specs, off, { status: 'disabled' })).toBe(true);
  });

  it('selects several statuses at once, which the old dropdown could not', () => {
    // The gain from the filter row: the toolbar's `<select>` held one value, so "everything that is
    // not currently running" took two passes and a memory.
    const ended = win({ active: false, ends_at: iso(-HOUR) });
    expect(keeps(specs, ended, { status: 'ended,disabled' })).toBe(true);
    expect(keeps(specs, win(), { status: 'ended,disabled' })).toBe(false);
  });

  it('separates the name from the scope, which one search box could not', () => {
    // The old `q` matched the name **and** the resolved scope, so "prod" found a window *named*
    // prod and a window *covering* prod identically and could not say which was meant.
    expect(keeps(specs, win(), { name: term('SWITCH') })).toBe(true);
    expect(keeps(specs, win(), { name: term('rtr-01') })).toBe(false);
    expect(keeps(specs, win(), { scope: term('rtr-01') })).toBe(true);
    expect(keeps(specs, win(), { scope: term('switch') })).toBe(false);
  });

  it('never resolves a scope name unless that column is narrowing', () => {
    // 🚨 The label goes through `useEntityNames`, whose resolver **enqueues every id it is asked
    // about** — so reading it per row unconditionally would fetch names for rows nobody is looking
    // for. The old code bought this with an early return inside `matchesWindow`; it is now a
    // property of `buildPredicate` compiling an inactive column to `null`. Same guarantee, but no
    // longer something each screen has to remember, which is why the test moved rather than went.
    const spy = vi.fn(label);
    const spied = windowFilters(t, spy, NOW);
    keeps(spied, win(), { status: 'active' });
    expect(spy).not.toHaveBeenCalled();
    keeps(spied, win(), { scope: term('rtr') });
    expect(spy).toHaveBeenCalled();
  });
});

describe('muteIsExpired', () => {
  it('treats the exact boundary as expired, like the server does', () => {
    // The server prunes on `until_at <= now()`; a `<` here would put a mute expiring exactly now on
    // the opposite side of the boundary from the server that is about to delete it.
    expect(muteIsExpired({ until_at: iso(0) }, NOW)).toBe(true);
    expect(muteIsExpired({ until_at: iso(1) }, NOW)).toBe(false);
    expect(muteIsExpired({ until_at: iso(-1) }, NOW)).toBe(true);
  });
});

describe('the mutes filter row', () => {
  const specs = muteFilters(t, label, NOW);

  it('shows everything when nothing is set', () => {
    expect(keeps(specs, mute(), {})).toBe(true);
  });

  it('splits active from expired, and each excludes the other', () => {
    const live = mute();
    const dead = mute({ until_at: iso(-HOUR) });
    expect(keeps(specs, live, { until: 'active' })).toBe(true);
    expect(keeps(specs, dead, { until: 'active' })).toBe(false);
    expect(keeps(specs, dead, { until: 'expired' })).toBe(true);
    expect(keeps(specs, live, { until: 'expired' })).toBe(false);
    expect(MUTE_STATES).toEqual(['active', 'expired']);
  });

  it('separates the target, the metric and the reason', () => {
    expect(keeps(specs, mute(), { target: term('RTR-01') })).toBe(true);
    expect(keeps(specs, mute(), { metric: term('icmp') })).toBe(true);
    expect(keeps(specs, mute(), { reason: term('noisy') })).toBe(true);
    // …and each one only answers about its own column, which the single search box could not.
    expect(keeps(specs, mute(), { reason: term('icmp') })).toBe(false);
    expect(keeps(specs, mute(), { metric: term('noisy') })).toBe(false);
  });

  it('ANDs the columns, so two conditions narrow rather than widen', () => {
    expect(keeps(specs, mute(), { metric: term('icmp'), reason: term('noisy') })).toBe(true);
    expect(keeps(specs, mute(), { metric: term('icmp'), reason: term('quiet') })).toBe(false);
  });

  it('survives a mute with no metric — the "all metrics" case', () => {
    // `metric_name` is null for a node-wide mute, and a filter must not turn that into a crash.
    const all = mute({ metric_name: null });
    expect(keeps(specs, all, { reason: term('noisy') })).toBe(true);
    expect(keeps(specs, all, { metric: term('icmp') })).toBe(false);
  });

  it('never resolves a target name unless that column is narrowing', () => {
    const spy = vi.fn(label);
    const spied = muteFilters(t, spy, NOW);
    keeps(spied, mute(), { until: 'active' });
    expect(spy).not.toHaveBeenCalled();
    keeps(spied, mute(), { target: term('rtr') });
    expect(spy).toHaveBeenCalled();
  });
});
