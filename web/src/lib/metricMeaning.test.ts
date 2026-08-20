// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  BUILTIN_METRICS,
  CHECK_METRICS,
  DERIVED_METRICS,
  EXPLAINED_METRICS,
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

  it("does not overlap Yagra's own check metrics", () => {
    // The two halves come from different worlds — poller-emitted vs SNMP-collected — and an
    // overlap would mean one of them is mislabelled at the source.
    for (const m of BUILTIN_METRICS) {
      expect(CHECK_METRICS as readonly string[]).not.toContain(m.metric_name);
    }
  });

  it('resolves a built-in by name and returns undefined for anything else', () => {
    expect(builtinMetric('if_oper_status')?.per_interface).toBe(true);
    expect(builtinMetric('snmp_sys_uptime_ticks')?.per_interface).toBe(false);
    expect(builtinMetric('acme_widget_temp')).toBeUndefined();
  });
});
