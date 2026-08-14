// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Alerts ▸ Routing filter rows (no DOM — Vitest node env).
//
// These tested `matchesChannel` / `matchesRoutingRule`, two hand-written row predicates that
// ADR-053 Inc.5 replaced with column specs run through the shared `buildPredicate`. What is tested
// now is what each column reads off a row — including the one rule that is genuinely about routing
// rather than about filtering, and which a shared predicate could not have inferred.

import { describe, expect, it } from 'vitest';
import { SEVERITIES, type NotificationChannel, type RoutingRule, type Severity } from '../types/api';
import { channelFilters, routingRuleFilters } from './routingFilters';
import { specColumns, type ColumnFilterSpec, type FilterState } from '../lib/columnFilter';
import { buildPredicate } from '../lib/filterPredicate';
import { encodeCondition } from '../lib/filterCondition';

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

const t = ((k: string) => k) as unknown as Parameters<typeof channelFilters>[0];
const sevLabel = (s: Severity) => s;
const term = (s: string) => encodeCondition({ term: s, mode: 'contains', not: false });

function keeps<T>(specs: Record<string, ColumnFilterSpec<T>>, row: T, state: FilterState): boolean {
  return buildPredicate(specColumns(specs), state, 0)(row);
}

describe('the channels filter row', () => {
  const specs = channelFilters(t, ['webhook', 'email']);

  it('shows everything when nothing is set', () => {
    expect(keeps(specs, chan(), {})).toBe(true);
  });

  it('filters by kind, by enabled and by name', () => {
    expect(keeps(specs, chan(), { kind: 'webhook' })).toBe(true);
    expect(keeps(specs, chan(), { kind: 'email' })).toBe(false);
    expect(keeps(specs, chan({ enabled: false }), { status: 'disabled' })).toBe(true);
    expect(keeps(specs, chan({ enabled: false }), { status: 'enabled' })).toBe(false);
    expect(keeps(specs, chan(), { name: term('OPS') })).toBe(true);
    expect(keeps(specs, chan(), { name: term('pager') })).toBe(false);
  });

  it('selects several kinds at once, which the old dropdown could not', () => {
    expect(keeps(specs, chan(), { kind: 'webhook,email' })).toBe(true);
    expect(keeps(specs, chan({ kind: 'email' }), { kind: 'webhook,email' })).toBe(true);
  });
});

describe('the routing-rules filter row', () => {
  const specs = routingRuleFilters(t, sevLabel);

  it('shows everything when nothing is set', () => {
    expect(keeps(specs, rule(), {})).toBe(true);
  });

  it('keeps an any-severity rule under every severity filter', () => {
    // 🚨 A rule with no severity of its own routes ALL of them. Hiding it under "critical" would
    // answer "what would page me for a critical?" by omitting the rules that always do — the one
    // domain rule on this screen that no shared predicate could infer, which is why it is a spec
    // that returns *every* severity rather than a null the predicate would drop.
    const any = rule({ severity: null });
    expect(keeps(specs, any, { severity: 'critical' })).toBe(true);
    expect(keeps(specs, any, { severity: 'info' })).toBe(true);
    // …and the array it returns is the whole vocabulary, so a fourth severity is covered without
    // this test or that spec being touched.
    const sev = specs.severity;
    expect(sev.kind === 'enum' && sev.readValue?.(any)).toEqual(SEVERITIES);
  });

  it('still excludes a rule bound to a different severity', () => {
    expect(keeps(specs, rule(), { severity: 'critical' })).toBe(true);
    expect(keeps(specs, rule(), { severity: 'info' })).toBe(false);
  });

  it('filters by enabled and by name', () => {
    expect(keeps(specs, rule({ enabled: false }), { status: 'disabled' })).toBe(true);
    expect(keeps(specs, rule({ enabled: false }), { status: 'enabled' })).toBe(false);
    expect(keeps(specs, rule(), { name: term('CRITICAL') })).toBe(true);
    expect(keeps(specs, rule(), { name: term('warn') })).toBe(false);
  });
});
