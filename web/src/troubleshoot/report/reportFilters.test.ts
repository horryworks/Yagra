// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the three Troubleshoot report bodies that gained a filter row (ADR-053 Inc.7).
// No DOM — Vitest runs in the node env, and the bodies themselves are `.tsx` and therefore
// unreachable, which is exactly why every decision they make lives in `reportFilters.ts`.

import { describe, expect, it } from 'vitest';
import type { AnalysisFinding } from '../../types/api';
import { EVENT_KINDS, FINDING_SEVERITIES } from '../../types/api';
import {
  authProbeColumns,
  authProbeFilterLabels,
  authProbeFilters,
  flowScanColumns,
  flowScanFilters,
  ruleGapColumns,
  ruleGapFilters,
} from './reportFilters';
import { SCAN_PATTERNS } from './format';
import {
  activeFilterCount,
  defaultFilters,
  isAnyFiltered,
  reservedKeyCollisions,
  type FilterState,
  type FilterableColumn,
} from '../../lib/columnFilter';
import { applyFilters, matchesFilters } from '../../lib/filterPredicate';
import { facetCounts } from '../../lib/filterCounts';

/** The identity `t` every spec test uses: a key stands for itself, so an assertion names the key. */
const t = ((k: string) => k) as unknown as Parameters<typeof ruleGapFilters>[0];
const NOW = Date.parse('2026-08-14T12:00:00Z');

const finding = (over: Partial<AnalysisFinding> = {}): AnalysisFinding =>
  ({
    id: 'f1',
    kind: 'rule_gap',
    metric: 'events',
    node_id: 'n1',
    node_name: 'core-sw-01',
    score: 50,
    severity: 'warn',
    at: '2026-08-14T11:00:00Z',
    detail: {},
    ...over,
  }) as AnalysisFinding;

const state = (cols: FilterableColumn<AnalysisFinding>[], over: Record<string, string>): FilterState => ({
  ...defaultFilters(cols),
  ...over,
});

// ---------------------------------------------------------------------------

describe('the rule-gap filter row', () => {
  const cols = ruleGapColumns(t);
  const gap = (detail: Record<string, unknown>, over: Partial<AnalysisFinding> = {}) =>
    finding({ detail: detail as AnalysisFinding['detail'], ...over });

  it('shows everything when nothing is set, and claims no reserved URL key', () => {
    expect(matchesFilters(gap({ signature: 'sshd', kind: 'syslog' }), cols, defaultFilters(cols), NOW)).toBe(true);
    expect(isAnyFiltered(cols, defaultFilters(cols))).toBe(false);
    expect(reservedKeyCollisions(cols)).toEqual([]);
  });

  it('offers the passive-event vocabulary, not the kinds this run happened to contain', () => {
    // A run with no traps must still let an operator ask for traps and be told "none" — options
    // that appear and vanish with the data cannot be reasoned about.
    const src = ruleGapFilters(t).src;
    expect(src.options.map((o) => o.value)).toEqual([...EVENT_KINDS]);
  });

  it('reads the source kind from `detail`, never from `finding.kind`', () => {
    // `finding.kind` is the literal 'rule_gap' on every row of this report; the two collide by name.
    const rows = [
      gap({ signature: 'a', kind: 'trap' }, { id: 'a' }),
      gap({ signature: 'b', kind: 'syslog' }, { id: 'b' }),
    ];
    expect(applyFilters(rows, cols, state(cols, { src: 'trap' }), NOW).map((r) => r.id)).toEqual(['a']);
    // The report's own `kind` value must not be selectable as a source.
    expect(applyFilters(rows, cols, state(cols, { src: 'rule_gap' }), NOW)).toEqual([]);
  });

  it('takes several source kinds at once — the thing the old single-valued select could not say', () => {
    const rows = [
      gap({ signature: 'a', kind: 'trap' }, { id: 'a' }),
      gap({ signature: 'b', kind: 'syslog' }, { id: 'b' }),
      gap({ signature: 'c', kind: 'webhook' }, { id: 'c' }),
    ];
    expect(applyFilters(rows, cols, state(cols, { src: 'syslog,trap' }), NOW).map((r) => r.id)).toEqual([
      'a',
      'b',
    ]);
  });

  it('falls back to the metric when a finding carries no signature', () => {
    const rows = [gap({ kind: 'syslog' }, { id: 'a', metric: 'ifOperStatus' })];
    expect(applyFilters(rows, cols, state(cols, { sig: 'ifOper' }), NOW).map((r) => r.id)).toEqual(['a']);
  });

  it('excludes a signature with NOT, and matches a pattern with regex', () => {
    const rows = [
      gap({ signature: 'sshd', kind: 'syslog' }, { id: 'a' }),
      gap({ signature: 'sudo', kind: 'syslog' }, { id: 'b' }),
    ];
    expect(applyFilters(rows, cols, state(cols, { sig: '!sshd' }), NOW).map((r) => r.id)).toEqual(['b']);
    expect(applyFilters(rows, cols, state(cols, { sig: '~^ss' }), NOW).map((r) => r.id)).toEqual(['a']);
  });

  it('narrows by event volume, inclusive at both ends and open on either side', () => {
    const rows = [
      gap({ signature: 'a', count: 5 }, { id: 'a' }),
      gap({ signature: 'b', count: 50 }, { id: 'b' }),
      gap({ signature: 'c', count: 500 }, { id: 'c' }),
    ];
    expect(applyFilters(rows, cols, state(cols, { count: '50:500' }), NOW).map((r) => r.id)).toEqual([
      'b',
      'c',
    ]);
    expect(applyFilters(rows, cols, state(cols, { count: '50:' }), NOW).map((r) => r.id)).toEqual(['b', 'c']);
    expect(applyFilters(rows, cols, state(cols, { count: ':50' }), NOW).map((r) => r.id)).toEqual(['a', 'b']);
  });

  it('matches a fleet-wide signature by the word the cell actually renders', () => {
    // The row shows the localized "fleet" label, not the backend's literal `node_name`. Typing what
    // is on screen has to work, or the filter is lying about the column it sits under.
    const rows = [
      gap({ signature: 'a' }, { id: 'a', node_id: null, node_name: 'fleet' }),
      gap({ signature: 'b' }, { id: 'b', node_id: 'n2', node_name: 'edge-rtr-02' }),
    ];
    expect(
      applyFilters(rows, cols, state(cols, { scope: 'report.rule_gap.fleet' }), NOW).map((r) => r.id),
    ).toEqual(['a']);
    expect(applyFilters(rows, cols, state(cols, { scope: 'edge' }), NOW).map((r) => r.id)).toEqual(['b']);
  });

  it('counts a source kind without counting its own filter', () => {
    // The autofilter rule: the counts beside `trap` must not collapse to 0 once `syslog` is picked.
    const rows = [
      gap({ signature: 'a', kind: 'trap' }, { id: 'a' }),
      gap({ signature: 'b', kind: 'syslog' }, { id: 'b' }),
    ];
    const counts = facetCounts(rows, cols, state(cols, { src: 'syslog' }), 'src', NOW);
    expect(counts.trap).toBe(1);
    expect(counts.syslog).toBe(1);
  });
});

// ---------------------------------------------------------------------------

describe('the flow-scan filter row', () => {
  const cols = flowScanColumns(t);
  const scan = (detail: Record<string, unknown>, over: Partial<AnalysisFinding> = {}) =>
    finding({ detail: detail as AnalysisFinding['detail'], ...over });

  it('shows everything when nothing is set, and claims no reserved URL key', () => {
    expect(matchesFilters(scan({ src: '10.0.0.1' }), cols, defaultFilters(cols), NOW)).toBe(true);
    expect(reservedKeyCollisions(cols)).toEqual([]);
  });

  it('gives every column a filter — this table is all numbers and identifiers', () => {
    expect(cols.map((c) => c.key)).toEqual(['src', 'node', 'dst', 'ports', 'flows', 'pattern', 'score']);
  });

  it('filters on the shape it recomputes, not on a stored field', () => {
    // The backend ships the shape inside an English `duration` string; the column renders
    // `scanPattern(dst, ports)` and the filter has to read the same recomputation.
    const rows = [
      scan({ src: 'a', distinct_dst: 4496, distinct_ports: 20 }, { id: 'sweep' }),
      scan({ src: 'b', distinct_dst: 20, distinct_ports: 85 }, { id: 'probe' }),
    ];
    expect(applyFilters(rows, cols, state(cols, { pattern: 'horizontal' }), NOW).map((r) => r.id)).toEqual([
      'sweep',
    ]);
    expect(applyFilters(rows, cols, state(cols, { pattern: 'vertical' }), NOW).map((r) => r.id)).toEqual([
      'probe',
    ]);
    expect(flowScanFilters(t).pattern.options.map((o) => o.value)).toEqual([...SCAN_PATTERNS]);
  });

  it('ties on the same side the Rust does', () => {
    // `scanPattern` is `dst >= ports`, tie included. A filter that split the tie the other way would
    // hide a row from the very control that says which shape it is.
    const rows = [scan({ src: 'a', distinct_dst: 30, distinct_ports: 30 }, { id: 'tie' })];
    expect(applyFilters(rows, cols, state(cols, { pattern: 'horizontal' }), NOW).map((r) => r.id)).toEqual([
      'tie',
    ]);
  });

  it('narrows by each numeric column independently', () => {
    const rows = [
      scan({ src: 'a', distinct_dst: 10, distinct_ports: 2, flows: 100 }, { id: 'a', score: 20 }),
      scan({ src: 'b', distinct_dst: 900, distinct_ports: 3, flows: 9000 }, { id: 'b', score: 90 }),
    ];
    expect(applyFilters(rows, cols, state(cols, { dst: '100:' }), NOW).map((r) => r.id)).toEqual(['b']);
    expect(applyFilters(rows, cols, state(cols, { flows: ':500' }), NOW).map((r) => r.id)).toEqual(['a']);
    expect(applyFilters(rows, cols, state(cols, { score: '80:100' }), NOW).map((r) => r.id)).toEqual(['b']);
  });

  it('searches the source address and the node name separately', () => {
    const rows = [
      scan({ src: '192.168.1.50' }, { id: 'a', node_name: 'edge-rtr-02' }),
      scan({ src: '10.9.9.9' }, { id: 'b', node_name: 'core-sw-01' }),
    ];
    expect(applyFilters(rows, cols, state(cols, { src: '192.168' }), NOW).map((r) => r.id)).toEqual(['a']);
    expect(applyFilters(rows, cols, state(cols, { node: 'core' }), NOW).map((r) => r.id)).toEqual(['b']);
    // Two columns narrow as a conjunction, not as a single free-text box over both.
    expect(applyFilters(rows, cols, state(cols, { src: '192.168', node: 'core' }), NOW)).toEqual([]);
  });
});

// ---------------------------------------------------------------------------

describe('the auth-probe filter bar', () => {
  const cols = authProbeColumns(t);
  const probe = (detail: Record<string, unknown>, over: Partial<AnalysisFinding> = {}) =>
    finding({ detail: detail as AnalysisFinding['detail'], ...over });

  it('shows everything when nothing is set, and claims no reserved URL key', () => {
    expect(matchesFilters(probe({ source_ip: '10.0.0.1' }), cols, defaultFilters(cols), NOW)).toBe(true);
    expect(reservedKeyCollisions(cols)).toEqual([]);
  });

  it('names every control — a bar has no header row above it to do that', () => {
    // `FilterBar` falls back to the raw key when a label is missing, which reads as a bug in the UI
    // and is invisible to tsc.
    const labels = authProbeFilterLabels(t);
    expect(Object.keys(labels).sort()).toEqual(cols.map((c) => c.key).sort());
  });

  it('offers all three severities, where the chip row it replaced offered two', () => {
    expect(authProbeFilters(t).severity.options.map((o) => o.value)).toEqual([...FINDING_SEVERITIES]);
  });

  it('says "critical and warning", which a single-valued chip row could not', () => {
    const rows = [
      probe({ source_ip: '1.1.1.1' }, { id: 'c', severity: 'crit' }),
      probe({ source_ip: '2.2.2.2' }, { id: 'w', severity: 'warn' }),
      probe({ source_ip: '3.3.3.3' }, { id: 'i', severity: 'info' }),
    ];
    expect(applyFilters(rows, cols, state(cols, { severity: 'crit,warn' }), NOW).map((r) => r.id)).toEqual([
      'c',
      'w',
    ]);
  });

  it('treats an unknown severity as info, exactly as the report renders it', () => {
    const rows = [probe({ source_ip: '4.4.4.4' }, { id: 'x', severity: 'catastrophic' })];
    expect(applyFilters(rows, cols, state(cols, { severity: 'info' }), NOW).map((r) => r.id)).toEqual(['x']);
  });

  it('excludes a subnet with NOT — the move triage actually makes here', () => {
    const rows = [
      probe({ source_ip: '10.1.0.7' }, { id: 'jump' }),
      probe({ source_ip: '203.0.113.9' }, { id: 'outside' }),
    ];
    expect(applyFilters(rows, cols, state(cols, { source: '!10.1.' }), NOW).map((r) => r.id)).toEqual([
      'outside',
    ]);
  });

  it('narrows by failure volume', () => {
    const rows = [
      probe({ source_ip: 'a', count: 3 }, { id: 'few' }),
      probe({ source_ip: 'b', count: 4000 }, { id: 'many' }),
    ];
    expect(applyFilters(rows, cols, state(cols, { count: '100:' }), NOW).map((r) => r.id)).toEqual(['many']);
  });

  it('counts one narrowed column once, however many dimensions its condition carries', () => {
    // The clear-all badge reads this; a regex-with-NOT is one filter, not three.
    expect(activeFilterCount(cols, state(cols, { source: '!~^10\\.' }))).toBe(1);
  });
});
