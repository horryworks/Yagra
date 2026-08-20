// SPDX-License-Identifier: AGPL-3.0-only
// Registry consistency + resolution rules for the Device-health gauges.
//
// The bug class here is not "a card renders wrong" — it is "the whole section disappears". Both
// render guards used to be hand-written lists of card names, so an entry missed in either one made
// Device health vanish for every node while still compiling. These tests pin the guards to the
// registry, and the resolution to the metric inventory.

import { describe, expect, it } from 'vitest';
import {
  claimedMetrics,
  hasAnyHealth,
  lastValue,
  MEM_SPECS,
  METRIC_CARDS,
  overviewScalarCards,
  resolveCard,
  resolveHealth,
  resolveMem,
} from './metricCards';
import type { NodeMetricEntry } from '../../types/api';

const entry = (metric: string, over: Partial<NodeMetricEntry> = {}): NodeMetricEntry => ({
  metric,
  metric_kind: 'gauge',
  dimension: 'none',
  status: 'ok',
  series_count: 1,
  ...over,
});

/** A per-entity source — what a table walk's series look like once they land. */
const table = (metric: string): NodeMetricEntry => entry(metric, { dimension: 'entity' });
/** A node-level source. */
const scalar = (metric: string): NodeMetricEntry => entry(metric);
/** A node-level counter — the shape that made `SETUP RATE` print an odometer as a rate. */
const counter = (metric: string): NodeMetricEntry => entry(metric, { metric_kind: 'counter' });

describe('METRIC_CARDS registry', () => {
  it('has a unique id and at least one candidate per card', () => {
    const ids = METRIC_CARDS.map((c) => c.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const c of METRIC_CARDS) expect(c.candidates.length).toBeGreaterThan(0);
  });

  it('never lets two cards claim the same metric', () => {
    // A metric listed twice would resolve on both cards and render the same gauge twice, which
    // reads to an operator as two different measurements that happen to always agree.
    const seen = new Map<string, string>();
    for (const c of METRIC_CARDS) {
      for (const m of c.candidates) {
        expect(seen.has(m), `${m} is claimed by both ${seen.get(m)} and ${c.id}`).toBe(false);
        seen.set(m, c.id);
      }
    }
  });

  it('keeps memory out of the single-metric cards', () => {
    // Memory is a derived pair, not a gauge; if one of its inputs ever appeared as a card candidate
    // the node would show a raw "free bytes" reading next to the derived usage card.
    // Widened to `string`: `as const satisfies` narrows both lists to their literals, so with the
    // sets disjoint today tsc rejects the comparison outright. That is the check passing early, not
    // a reason to skip it — the runtime assertion is what catches a future overlap.
    const cardMetrics = new Set<string>(METRIC_CARDS.flatMap((c) => [...c.candidates]));
    for (const spec of MEM_SPECS) {
      for (const m of spec.metrics) expect(cardMetrics.has(m)).toBe(false);
    }
  });
});

describe('resolveCard', () => {
  it('takes the first candidate the node collects, not the first it finds', () => {
    const spec = METRIC_CARDS.find((c) => c.id === 'cpu')!;
    const [first, second] = spec.candidates;
    // Both present → priority order decides.
    expect(resolveCard([table(second), table(first)], spec.candidates)?.metric).toBe(first);
    // Only the lower-priority one present → that one.
    expect(resolveCard([table(second)], spec.candidates)?.metric).toBe(second);
  });

  it('aggregates node-wide for per-entity sources and not for node-level ones', () => {
    // A per-entity metric has one series per row (per-CPU, per-VDOM, per-context); without `max`
    // the read would return an arbitrary row. A node-level metric is already one series.
    expect(resolveCard([table('a')], ['a'])).toEqual({
      metric: 'a',
      read: { kind: 'aggregate' },
      chart: { kind: 'aggregate' },
    });
    expect(resolveCard([scalar('a')], ['a'])).toEqual({
      metric: 'a',
      read: { kind: 'latest' },
      chart: { kind: 'range' },
    });
  });

  it('charts a counter as a rate and refuses to read its stored value', () => {
    // The regression this file exists for since ADR-046 Inc.6. `resolveCard` used to derive its
    // whole plan from `dimension`, so a counter candidate resolved to a plain range over the
    // stored odometer — which the `setupRate` card then labelled "/s". On the real firewall that
    // printed 18,190,268/s for a device opening a few sessions a second, as a straight rising
    // line that looks exactly like a working chart. `read: none` is the other half: there is no
    // current value to fetch, so the headline has to come from the rate series.
    expect(resolveCard([counter('a')], ['a'])).toEqual({
      metric: 'a',
      read: { kind: 'none' },
      chart: { kind: 'rate' },
    });
  });

  it('skips a candidate it cannot draw and lets the next one win', () => {
    // Two cells of `metricView` have no query behind them. A per-entity counter would have to be
    // differentiated per row and then collapsed, and a folded multi-index table's rows cannot be
    // named; a per-interface metric belongs to the Interfaces tab, which shows every row by name.
    // Returning either would produce a headline over a permanently empty chart.
    expect(resolveCard([entry('a', { metric_kind: 'counter', dimension: 'entity' })], ['a'])).toBeNull();
    expect(resolveCard([entry('a', { dimension: 'interface' })], ['a'])).toBeNull();
    expect(
      resolveCard([entry('a', { dimension: 'interface' }), scalar('b')], ['a', 'b'])?.metric,
    ).toBe('b');
  });

  it('resolves to null when the node has none of the candidates', () => {
    expect(resolveCard([table('something_else')], ['a', 'b'])).toBeNull();
  });

  it('skips a candidate that is configured but has never reported', () => {
    // The inventory sees these where the collection set could not tell them apart from a working
    // one. A card for a silent metric is a permanent dash over an empty chart, which reads as a
    // broken widget rather than as a device that has nothing to say.
    expect(resolveCard([entry('a', { status: 'no_data', series_count: 0 })], ['a'])).toBeNull();
    // ...and the next candidate down is then free to win.
    expect(
      resolveCard([entry('a', { status: 'no_data', series_count: 0 }), scalar('b')], ['a', 'b'])
        ?.metric,
    ).toBe('b');
  });

  it('accepts a metric that has data but no collection item', () => {
    // Check-spec metrics never appear in a collection set at all, so a source-agnostic rule is the
    // only one that can ever see them.
    expect(resolveCard([entry('a', { status: 'unconfigured' })], ['a'])?.metric).toBe('a');
  });
});

describe('resolveMem', () => {
  it('needs both inputs of a source — used+total cannot be derived from one', () => {
    const huawei = MEM_SPECS[0];
    expect(resolveMem([table(huawei.metrics[0])])).toBeNull();
    expect(resolveMem(huawei.metrics.map(table))?.id).toBe(huawei.id);
  });

  it('honours source priority when a node collects more than one pair', () => {
    const [first, second] = MEM_SPECS;
    const items = [...first.metrics, ...second.metrics].map(table);
    expect(resolveMem(items)?.id).toBe(first.id);
  });
});

describe('resolveHealth', () => {
  it('answers for every registered card, so neither render guard can miss one', () => {
    const health = resolveHealth([]);
    expect(Object.keys(health.cards).sort()).toEqual(METRIC_CARDS.map((c) => c.id).sort());
  });

  it('hides the section only when the node has nothing at all', () => {
    expect(hasAnyHealth(resolveHealth([]))).toBe(false);
    // Any single card is enough to show the section...
    for (const spec of METRIC_CARDS) {
      expect(hasAnyHealth(resolveHealth([table(spec.candidates[0])]))).toBe(true);
    }
    // ...and so is memory alone, on a node with no gauges.
    expect(hasAnyHealth(resolveHealth(MEM_SPECS[0].metrics.map(table)))).toBe(true);
  });

  it('resolves cards independently of one another', () => {
    const cpu = METRIC_CARDS.find((c) => c.id === 'cpu')!;
    const vpn = METRIC_CARDS.find((c) => c.id === 'vpnTunnels')!;
    const health = resolveHealth([table(cpu.candidates[0]), scalar(vpn.candidates[0])]);
    expect(health.cards.cpu?.metric).toBe(cpu.candidates[0]);
    expect(health.cards.vpnTunnels?.read).toEqual({ kind: 'latest' });
    expect(health.cards.sessions).toBeNull();
    expect(health.mem).toBeNull();
  });
});

describe('claimedMetrics', () => {
  it('counts the memory pair, which no card names as its own metric', () => {
    // MEMORY is derived from `*_total` + `*_free`; neither is a `cards[*].metric`, so counting the
    // card map alone would leave both raw byte gauges free to reappear as their own cards directly
    // under the card that is made of them.
    const mem = MEM_SPECS[0];
    const claimed = claimedMetrics(resolveHealth(mem.metrics.map(table)));
    for (const m of mem.metrics) expect(claimed.has(m)).toBe(true);
  });

  it('names the candidate that actually resolved, not the whole candidate list', () => {
    const cpu = METRIC_CARDS.find((c) => c.id === 'cpu')!;
    const [first, second] = cpu.candidates;
    const claimed = claimedMetrics(resolveHealth([table(second)]));
    expect(claimed.has(second)).toBe(true);
    expect(claimed.has(first)).toBe(false);
  });
});

describe('overviewScalarCards', () => {
  it('subtracts what Device health already draws', () => {
    const cpu = METRIC_CARDS.find((c) => c.id === 'cpu')!;
    const items = [table(cpu.candidates[0]), scalar('icmp_rtt_ms'), ...MEM_SPECS[0].metrics.map(table)];
    const names = overviewScalarCards(items, resolveHealth(items)).map((c) => c.metric);
    // The CPU gauge and both memory inputs are charted above; only the leftover is a card here.
    expect(names).toEqual(['icmp_rtt_ms']);
  });

  it('carries each metric its own read and chart, so the card never picks', () => {
    const items = [scalar('a'), table('b')];
    expect(overviewScalarCards(items, resolveHealth(items))).toEqual([
      { metric: 'a', read: { kind: 'latest' }, chart: { kind: 'range' } },
      { metric: 'b', read: { kind: 'aggregate' }, chart: { kind: 'aggregate' } },
    ]);
  });

  it('drops counters and per-interface metrics, as the Overview always has', () => {
    // Not new in Inc.6 — `overviewScalars` has always refused these. Pinned here because this is
    // now the predicate the section renders from, and widening it would put eight octet counters
    // above the fold on every switch.
    const items = [
      counter('c'),
      entry('i', { dimension: 'interface' }),
      entry('silent', { status: 'no_data', series_count: 0 }),
      scalar('keep'),
    ];
    expect(overviewScalarCards(items, resolveHealth(items)).map((c) => c.metric)).toEqual(['keep']);
  });

  it('subtracts nothing while Device health is still resolving', () => {
    // The caller does not render in this state; if it did, drawing the unsubtracted set first would
    // flash the duplicates for one frame.
    const cpu = METRIC_CARDS.find((c) => c.id === 'cpu')!;
    const items = [table(cpu.candidates[0])];
    expect(overviewScalarCards(items, null).map((c) => c.metric)).toEqual([cpu.candidates[0]]);
  });
});

describe('lastValue', () => {
  it('takes the last real sample — a counter card has no other headline', () => {
    expect(lastValue([1, 2, 3])).toBe(3);
    expect(lastValue([])).toBeNull();
    // VictoriaMetrics answers a gap-free array, but a rate over a window with no second sample
    // can still produce NaN — which would render as "NaN/s" rather than as "no reading".
    expect(lastValue([1, Number.NaN])).toBe(1);
    expect(lastValue([Number.NaN])).toBeNull();
    // Zero is a reading, not a missing one. A quiet firewall's setup rate is 0/s.
    expect(lastValue([5, 0])).toBe(0);
  });
});
