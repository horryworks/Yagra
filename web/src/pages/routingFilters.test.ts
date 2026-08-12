// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Alerts ▸ Routing filters (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { NotificationChannel, RoutingRule } from '../types/api';
import {
  DEFAULT_CHANNEL_FILTERS,
  DEFAULT_ROUTING_FILTERS,
  isChannelFiltered,
  isRoutingFiltered,
  matchesChannel,
  matchesRoutingRule,
  type ChannelFilters,
  type RoutingFilters,
} from './routingFilters';

const chan = (over: Partial<NotificationChannel> = {}): NotificationChannel => ({
  id: 'c1',
  name: 'ops webhook',
  kind: 'webhook',
  enabled: true,
  subject_template: null,
  body_template: null,
  ...over,
});

const rule = (over: Partial<RoutingRule> = {}): RoutingRule => ({
  id: 'r1',
  name: 'page on critical',
  severity: 'critical',
  enabled: true,
  channel_ids: ['c1'],
  ...over,
});

const cf = (over: Partial<ChannelFilters>): ChannelFilters => ({ ...DEFAULT_CHANNEL_FILTERS, ...over });
const rf = (over: Partial<RoutingFilters>): RoutingFilters => ({ ...DEFAULT_ROUTING_FILTERS, ...over });

describe('matchesChannel', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesChannel(chan(), DEFAULT_CHANNEL_FILTERS)).toBe(true);
  });

  it('filters by kind, by enabled and by name', () => {
    expect(matchesChannel(chan(), cf({ kind: 'webhook' }))).toBe(true);
    expect(matchesChannel(chan(), cf({ kind: 'email' }))).toBe(false);
    expect(matchesChannel(chan({ enabled: false }), cf({ enabled: 'disabled' }))).toBe(true);
    expect(matchesChannel(chan({ enabled: false }), cf({ enabled: 'enabled' }))).toBe(false);
    expect(matchesChannel(chan(), cf({ q: 'OPS' }))).toBe(true);
    expect(matchesChannel(chan(), cf({ q: 'pager' }))).toBe(false);
  });

  it('flips isFiltered for every field', () => {
    expect(isChannelFiltered(DEFAULT_CHANNEL_FILTERS)).toBe(false);
    for (const f of [cf({ kind: 'email' }), cf({ enabled: 'disabled' }), cf({ q: 'x' })]) {
      expect(isChannelFiltered(f)).toBe(true);
    }
  });
});

describe('matchesRoutingRule', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesRoutingRule(rule(), DEFAULT_ROUTING_FILTERS)).toBe(true);
  });

  it('keeps an any-severity rule under every severity filter', () => {
    // A rule with no severity of its own routes ALL of them. Hiding it under "critical" would
    // answer "what would page me for a critical?" by omitting the rules that always do.
    const any = rule({ severity: null });
    expect(matchesRoutingRule(any, rf({ severity: 'critical' }))).toBe(true);
    expect(matchesRoutingRule(any, rf({ severity: 'info' }))).toBe(true);
  });

  it('still excludes a rule bound to a different severity', () => {
    expect(matchesRoutingRule(rule(), rf({ severity: 'critical' }))).toBe(true);
    expect(matchesRoutingRule(rule(), rf({ severity: 'info' }))).toBe(false);
  });

  it('filters by enabled and by name', () => {
    expect(matchesRoutingRule(rule({ enabled: false }), rf({ enabled: 'disabled' }))).toBe(true);
    expect(matchesRoutingRule(rule({ enabled: false }), rf({ enabled: 'enabled' }))).toBe(false);
    expect(matchesRoutingRule(rule(), rf({ q: 'CRITICAL' }))).toBe(true);
    expect(matchesRoutingRule(rule(), rf({ q: 'warn' }))).toBe(false);
  });

  it('flips isFiltered for every field', () => {
    expect(isRoutingFiltered(DEFAULT_ROUTING_FILTERS)).toBe(false);
    for (const f of [rf({ severity: 'info' }), rf({ enabled: 'disabled' }), rf({ q: 'x' })]) {
      expect(isRoutingFiltered(f)).toBe(true);
    }
  });
});
