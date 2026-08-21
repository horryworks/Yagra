// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the passive-event configuration filter rows (no DOM — Vitest node env).
//
// The claim this module makes in its own header is that the two screens share one enabled/disabled
// spec so that "a URL taken on one screen means the same thing on another". That is a statement
// about two things agreeing, which is the kind nothing else checks — so it is the first test here.

import { describe, expect, it } from 'vitest';
import type { EventRule, EventSource } from '../types/api';
import { eventRuleFilters, eventSourceFilters } from './eventConfigFilters';
import { specColumns, type ColumnFilterSpec, type FilterState } from '../lib/columnFilter';
import { buildPredicate } from '../lib/filterPredicate';
import { encodeCondition } from '../lib/filterCondition';
import { severityLabel } from '../lib/format';

const t = ((k: string) => k) as unknown as Parameters<typeof eventSourceFilters>[0];
const term = (s: string) => encodeCondition({ term: s, mode: 'contains', not: false });

const source = (over: Partial<EventSource> = {}): EventSource => ({
  id: 's1',
  name: 'edge syslog',
  kind: 'syslog',
  enabled: true,
  node_id: null,
  created_at: '2026-08-01T00:00:00Z',
  ...over,
});

const rule = (over: Partial<EventRule> = {}): EventRule => ({
  id: 'r1',
  name: 'link down',
  pattern: 'LINK-3-UPDOWN',
  clear_pattern: null,
  match_kind: 'substring',
  severity: 'critical',
  enabled: true,
  node_id: null,
  source_id: null,
  source_kind: null,
  min_count: 1,
  window_secs: 60,
  ttl_secs: 3600,
  created_at: '2026-08-01T00:00:00Z',
  ...over,
});

const sourceSpecs = eventSourceFilters(t, ['syslog', 'trap']);
const ruleSpecs = eventRuleFilters(t, ['info', 'warning', 'critical'], (r) =>
  r.node_id ? `node ${r.node_id}` : 'All nodes',
);

function keeps<T>(specs: Record<string, ColumnFilterSpec<T>>, row: T, state: FilterState): boolean {
  return buildPredicate(specColumns(specs), state, 0)(row);
}

describe('the enabled column means the same thing on both screens', () => {
  it('offers the same two values, in the same order', () => {
    // Labels differ (each namespace spells the words in its own keys) and are allowed to. The
    // values are what a URL carries, so those must not differ.
    for (const specs of [sourceSpecs, ruleSpecs]) {
      const status = specs.status;
      if (status.kind !== 'enum') throw new Error('status must stay an enum column');
      expect(status.options.map((o) => o.value)).toEqual(['enabled', 'disabled']);
    }
  });

  it('reads the boolean the same way on both screens', () => {
    expect(keeps(sourceSpecs, source({ enabled: false }), { status: 'disabled' })).toBe(true);
    expect(keeps(sourceSpecs, source({ enabled: false }), { status: 'enabled' })).toBe(false);
    expect(keeps(ruleSpecs, rule({ enabled: false }), { status: 'disabled' })).toBe(true);
    expect(keeps(ruleSpecs, rule({ enabled: false }), { status: 'enabled' })).toBe(false);
    // …and the accepting direction, or a spec that rejected everything would pass the four above.
    expect(keeps(sourceSpecs, source({ enabled: true }), { status: 'enabled' })).toBe(true);
    expect(keeps(ruleSpecs, rule({ enabled: true }), { status: 'enabled' })).toBe(true);
  });
});

describe('event sources', () => {
  it('shows everything when nothing is set', () => {
    expect(keeps(sourceSpecs, source(), {})).toBe(true);
  });

  it('filters by name and by kind', () => {
    expect(keeps(sourceSpecs, source(), { name: term('EDGE') })).toBe(true);
    expect(keeps(sourceSpecs, source(), { name: term('core') })).toBe(false);
    expect(keeps(sourceSpecs, source(), { kind: 'syslog' })).toBe(true);
    expect(keeps(sourceSpecs, source(), { kind: 'trap' })).toBe(false);
    expect(keeps(sourceSpecs, source({ kind: 'trap' }), { kind: 'syslog,trap' })).toBe(true);
  });

  it('offers a kind this build has never heard of', () => {
    // The option list is built from the rows, not from a hardcoded union, so a source created by a
    // newer core stays selectable rather than vanishing from the filter.
    //
    // The cast is the point of the test, not a shortcut: `EventSource['kind']` is a *typed* union
    // in the generated contract, and a TypeScript union is a compile-time claim — a core one
    // version ahead still puts the string on the wire and `JSON.parse` still hands it over. This
    // is what that row looks like.
    const withNew = eventSourceFilters(t, ['syslog', 'netflow-v9']);
    const kind = withNew.kind;
    if (kind.kind !== 'enum') throw new Error('kind must stay an enum column');
    expect(kind.options.map((o) => o.value)).toEqual(['syslog', 'netflow-v9']);
    const fromNewerCore = source({ kind: 'netflow-v9' as EventSource['kind'] });
    expect(keeps(withNew, fromNewerCore, { kind: 'netflow-v9' })).toBe(true);
    expect(keeps(withNew, fromNewerCore, { kind: 'syslog' })).toBe(false);
  });
});

describe('event rules', () => {
  // 🚨 The severity dropdown must name the severities the way the rest of the app does.
  //
  // It built its own key — `t('severity.critical')` — and this module is called with `t` bound to
  // `alertsConfig`, whose top level has no `severity` block. So the filter listed
  // `severity.critical` beside a badge in the same column reading `Critical`. EN/JA parity cannot
  // catch that: the key is missing from both locales equally, so parity passes.
  //
  // This is not a tautology even though the implementation now calls `severityLabel`, because the
  // `t` above is a fake that returns its key verbatim. A revert to the built key produces
  // `severity.critical` here while `severityLabel` produces the locale string, and the two differ.
  it('names the severities the way the rest of the app names them', () => {
    const spec = ruleSpecs.severity as Extract<ColumnFilterSpec<EventRule>, { kind: 'enum' }>;
    const labels = spec.options.map((o) => o.label);
    expect(labels).toEqual(['info', 'warning', 'critical'].map((v) => severityLabel(v as never)));
    expect(labels.some((l) => l.startsWith('severity.'))).toBe(false);
  });

  it('shows everything when nothing is set', () => {
    expect(keeps(ruleSpecs, rule(), {})).toBe(true);
  });

  it('filters by name, severity and scope', () => {
    expect(keeps(ruleSpecs, rule(), { name: term('LINK') })).toBe(true);
    expect(keeps(ruleSpecs, rule(), { severity: 'critical' })).toBe(true);
    expect(keeps(ruleSpecs, rule(), { severity: 'info,warning' })).toBe(false);
    expect(keeps(ruleSpecs, rule(), { scope: term('all nodes') })).toBe(true);
    expect(keeps(ruleSpecs, rule({ node_id: 'n-7' }), { scope: term('all nodes') })).toBe(false);
    expect(keeps(ruleSpecs, rule({ node_id: 'n-7' }), { scope: term('n-7') })).toBe(true);
  });

  it('searches the clear pattern as well as the fire pattern', () => {
    // A term that found only the fire half would hide half of what the rule matches on — which is
    // the answer you are after when a rule has stopped clearing.
    const both = rule({ pattern: 'LINK-3-UPDOWN.*down', clear_pattern: 'LINK-3-UPDOWN.*up' });
    expect(keeps(ruleSpecs, both, { pattern: term('down') })).toBe(true);
    expect(keeps(ruleSpecs, both, { pattern: term('up') })).toBe(true);
  });

  it('does not let a term match across the gap between the two patterns', () => {
    const both = rule({ pattern: 'alpha', clear_pattern: 'beta' });
    expect(keeps(ruleSpecs, both, { pattern: term('alpha') })).toBe(true);
    expect(keeps(ruleSpecs, both, { pattern: term('alphabeta') })).toBe(false);
  });

  it('treats an absent clear pattern as empty rather than as the string "null"', () => {
    expect(keeps(ruleSpecs, rule({ clear_pattern: null }), { pattern: term('null') })).toBe(false);
  });

  it('accepts a regular expression over the pattern, which is why regex is offered here', () => {
    const anchored = encodeCondition({ term: '^LINK', mode: 'regex', not: false });
    expect(keeps(ruleSpecs, rule({ pattern: 'LINK-3-UPDOWN' }), { pattern: anchored })).toBe(true);
    expect(keeps(ruleSpecs, rule({ pattern: 'IF-LINK-3' }), { pattern: anchored })).toBe(false);
  });
});
