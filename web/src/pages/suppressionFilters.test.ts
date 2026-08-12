// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the two suppression screens' filters (no DOM — Vitest node env).

import { describe, expect, it, vi } from 'vitest';
import type { MaintenanceWindow, Mute } from '../types/api';
import {
  DEFAULT_MAINTENANCE_FILTERS,
  DEFAULT_MUTE_FILTERS,
  isMaintenanceFiltered,
  isMuteFiltered,
  MAINTENANCE_STATUS_FILTERS,
  matchesMute,
  matchesWindow,
  muteIsExpired,
  MUTE_STATES,
  type MaintenanceFilters,
  type MuteFilters,
} from './suppressionFilters';
import { MAINTENANCE_STATUSES } from './maintenanceStatus';

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
const wFilters = (over: Partial<MaintenanceFilters>): MaintenanceFilters => ({
  ...DEFAULT_MAINTENANCE_FILTERS,
  ...over,
});
const mFilters = (over: Partial<MuteFilters>): MuteFilters => ({ ...DEFAULT_MUTE_FILTERS, ...over });

describe('the maintenance status vocabulary', () => {
  it('is the badge column\'s own, not a second list', () => {
    // A status the badge can render but the dropdown cannot select would be a row an operator can
    // see and not filter for.
    expect(MAINTENANCE_STATUS_FILTERS).toBe(MAINTENANCE_STATUSES);
  });
});

describe('matchesWindow', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesWindow(win(), DEFAULT_MAINTENANCE_FILTERS, label, NOW)).toBe(true);
  });

  it('filters on the same status the badge shows', () => {
    const running = win();
    const ended = win({ active: false, ends_at: iso(-HOUR) });
    const off = win({ enabled: false, active: false, ends_at: iso(HOUR) });
    expect(matchesWindow(running, wFilters({ status: 'active' }), label, NOW)).toBe(true);
    expect(matchesWindow(running, wFilters({ status: 'ended' }), label, NOW)).toBe(false);
    expect(matchesWindow(ended, wFilters({ status: 'ended' }), label, NOW)).toBe(true);
    expect(matchesWindow(off, wFilters({ status: 'disabled' }), label, NOW)).toBe(true);
  });

  it('searches the name and the resolved scope', () => {
    expect(matchesWindow(win(), wFilters({ q: 'SWITCH' }), label, NOW)).toBe(true);
    expect(matchesWindow(win(), wFilters({ q: 'rtr-01' }), label, NOW)).toBe(true);
    expect(matchesWindow(win(), wFilters({ q: 'sw-99' }), label, NOW)).toBe(false);
  });

  it('never resolves a scope name unless a term was typed', () => {
    // The label goes through `useEntityNames`, whose resolver enqueues every id it is asked about.
    const spy = vi.fn(label);
    matchesWindow(win(), wFilters({ status: 'active' }), spy, NOW);
    expect(spy).not.toHaveBeenCalled();
    matchesWindow(win(), wFilters({ q: 'rtr' }), spy, NOW);
    expect(spy).toHaveBeenCalled();
  });

  it('flips isFiltered for every field', () => {
    expect(isMaintenanceFiltered(DEFAULT_MAINTENANCE_FILTERS)).toBe(false);
    expect(isMaintenanceFiltered(wFilters({ status: 'ended' }))).toBe(true);
    expect(isMaintenanceFiltered(wFilters({ q: 'x' }))).toBe(true);
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

describe('matchesMute', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesMute(mute(), DEFAULT_MUTE_FILTERS, label, NOW)).toBe(true);
  });

  it('splits active from expired, and each excludes the other', () => {
    const live = mute();
    const dead = mute({ until_at: iso(-HOUR) });
    expect(matchesMute(live, mFilters({ state: 'active' }), label, NOW)).toBe(true);
    expect(matchesMute(dead, mFilters({ state: 'active' }), label, NOW)).toBe(false);
    expect(matchesMute(dead, mFilters({ state: 'expired' }), label, NOW)).toBe(true);
    expect(matchesMute(live, mFilters({ state: 'expired' }), label, NOW)).toBe(false);
    expect(MUTE_STATES).toEqual(['active', 'expired']);
  });

  it('searches the target, the metric and the reason', () => {
    expect(matchesMute(mute(), mFilters({ q: 'RTR-01' }), label, NOW)).toBe(true);
    expect(matchesMute(mute(), mFilters({ q: 'icmp' }), label, NOW)).toBe(true);
    expect(matchesMute(mute(), mFilters({ q: 'noisy' }), label, NOW)).toBe(true);
    expect(matchesMute(mute(), mFilters({ q: 'quiet' }), label, NOW)).toBe(false);
  });

  it('survives a mute with no metric — the "all metrics" case', () => {
    // `metric_name` is null for a node-wide mute, and a filter must not turn that into a crash.
    const all = mute({ metric_name: null });
    expect(matchesMute(all, mFilters({ q: 'noisy' }), label, NOW)).toBe(true);
    expect(matchesMute(all, mFilters({ q: 'icmp' }), label, NOW)).toBe(false);
  });

  it('flips isFiltered for every field', () => {
    expect(isMuteFiltered(DEFAULT_MUTE_FILTERS)).toBe(false);
    expect(isMuteFiltered(mFilters({ state: 'expired' }))).toBe(true);
    expect(isMuteFiltered(mFilters({ q: 'x' }))).toBe(true);
  });
});
