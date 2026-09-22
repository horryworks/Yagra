// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  BUILTIN_METRICS,
  CHECK_METRICS,
  DERIVED_METRICS,
  EXPLAINED_METRICS,
  OVERVIEW_FAMILIES,
  STANDARD_SNMP_TEMPLATE,
  builtinMetric,
  metricMeaningKey,
} from './metricMeaning';
import { LIVENESS_METRIC } from './format';
import { METRIC_PRESETS } from './suppression';

describe('metricMeaningKey', () => {
  it('answers for every metric it claims to explain', () => {
    for (const m of EXPLAINED_METRICS) {
      expect(metricMeaningKey(m)).toBe(`metricMeanings:${m}`);
    }
  });

  it('explains the liveness sentinel, which is the one an operator cannot look up anywhere else', () => {
    // It is not a series and has no collection item, so the rule table and the picker are the only
    // places it is ever described.
    expect(metricMeaningKey(LIVENESS_METRIC)).toBe(`metricMeanings:${LIVENESS_METRIC}`);
  });

  it('says nothing rather than something empty about a metric it does not know', () => {
    // `null` is what lets the callers do better than a sentence could: the table shows an em dash,
    // the picker shows the catalog facts it does have.
    expect(metricMeaningKey('acme_widget_temp')).toBeNull();
    expect(metricMeaningKey('')).toBeNull();
  });

  it('covers every metric the shared preset list still offers', () => {
    // The presets are what the dashboard's metric-top widget suggests, so they are names an
    // operator meets bare. Pinned as a relation rather than a copied list.
    expect(METRIC_PRESETS.filter((m) => metricMeaningKey(m) === null)).toEqual([]);
  });

  it('lists no metric twice', () => {
    // The array is iterated by the i18n coverage test; a duplicate would pass every check while
    // hiding that the two halves overlap.
    expect(new Set(EXPLAINED_METRICS).size).toBe(EXPLAINED_METRICS.length);
  });

  it('explains every metric the picker groups under a heading', () => {
    // `CHECK_METRICS` and `DERIVED_METRICS` are a WebUI concern — they decide which heading a
    // name appears under — but the sentences are owned by Rust since ADR-079. If the two lists
    // drift, the picker offers a name under a heading with an em dash where its explanation
    // should be, which is precisely the state the picker exists to prevent (a threshold on
    // `bgp_peer_state` is `above 3` or `below 3` depending on a fact no OID carries).
    //
    // A subset relation rather than equality: the explained set is much larger, because it also
    // covers every gauge in the collection catalogue.
    const ungrouped = [...CHECK_METRICS, ...DERIVED_METRICS].filter(
      (m) => !EXPLAINED_METRICS.includes(m),
    );
    expect(ungrouped).toEqual([]);
  });
});

describe('the generated built-in catalog', () => {
  it('is the second half of the explained set, and only its gauges', () => {
    // A counter can carry no rule (ADR-012), so it is never shown and owes no sentence — but it
    // must still be in the generated file, because the picker uses the kind to drop it.
    const gauges = BUILTIN_METRICS.filter((m) => m.metric_kind === 'gauge').map(
      (m) => m.metric_name,
    );
    const counters = BUILTIN_METRICS.filter((m) => m.metric_kind === 'counter').map(
      (m) => m.metric_name,
    );
    expect(counters.length).toBeGreaterThan(0);
    for (const g of gauges) expect(EXPLAINED_METRICS).toContain(g);
    for (const c of counters) expect(EXPLAINED_METRICS).not.toContain(c);
  });

  it("files every row under a source, and derives Yagra's own check list from the check rows", () => {
    // The two halves come from different worlds — poller-emitted vs SNMP-collected — and the
    // generated file says which is which. `CHECK_METRICS` is no longer a hand-written copy: it
    // is those rows, with the liveness sentinel (a rule token, not a series) put in front.
    for (const m of BUILTIN_METRICS) expect(['check', 'collected']).toContain(m.source);
    const checks = BUILTIN_METRICS.filter((m) => m.source === 'check').map((m) => m.metric_name);
    // 27 = the 28 rows of `metric_meaning.rs::CHECK_FAMILIES` minus `__liveness__`.
    expect(checks).toHaveLength(27);
    expect(checks).not.toContain(LIVENESS_METRIC);
    for (const m of BUILTIN_METRICS) {
      if (m.source === 'check') expect(OVERVIEW_FAMILIES).toContain(m.family);
    }
    expect(CHECK_METRICS[0]).toBe(LIVENESS_METRIC);
    expect([...CHECK_METRICS].sort()).toEqual([LIVENESS_METRIC, ...checks].sort());
    // Family order first, so the picker's "Yagra's own checks" group still reads probe by probe
    // rather than in the file's alphabetical order.
    const families = CHECK_METRICS.slice(1).map((m) => builtinMetric(m)?.family);
    expect(families.indexOf('snmp')).toBeGreaterThan(families.lastIndexOf('icmp'));
    expect(families.indexOf('url')).toBeGreaterThan(families.lastIndexOf('snmp'));
  });

  it('carries the metric set each collected row belongs to (ADR-046 Inc.8)', () => {
    expect(builtinMetric('huawei_temp')?.family).toBe('Huawei VRP health');
    expect(builtinMetric('snmp_sys_uptime_ticks')?.family).toBe(STANDARD_SNMP_TEMPLATE);
    expect(builtinMetric('icmp_loss_pct')?.family).toBe('icmp');
    // `STANDARD_SNMP_TEMPLATE` mirrors one Rust literal (`TEMPLATE_STANDARD_SNMP`). A rename there
    // must fail here rather than silently move sysUpTime out of the Overview's SNMP section.
    expect(
      BUILTIN_METRICS.some((m) => m.source === 'collected' && m.family === STANDARD_SNMP_TEMPLATE),
    ).toBe(true);
    for (const m of BUILTIN_METRICS) expect(m.family, m.metric_name).not.toBe('');
  });

  it('resolves a built-in by name and returns undefined for anything else', () => {
    expect(builtinMetric('if_oper_status')?.per_interface).toBe(true);
    expect(builtinMetric('snmp_sys_uptime_ticks')?.per_interface).toBe(false);
    expect(builtinMetric('acme_widget_temp')).toBeUndefined();
  });
});
