// SPDX-License-Identifier: AGPL-3.0-only
// Registry consistency + resolution rules for the Device-health gauges.
//
// The bug class here is not "a card renders wrong" — it is "the whole section disappears". Both
// render guards used to be hand-written lists of card names, so an entry missed in either one made
// Device health vanish for every node while still compiling. These tests pin the guards to the
// registry, and the resolution to the metric inventory.

import { describe, expect, it } from 'vitest';
import {
  hasAnyHealth,
  MEM_SPECS,
  METRIC_CARDS,
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
    expect(resolveCard([table('a')], ['a'])).toEqual({ metric: 'a', agg: 'max' });
    expect(resolveCard([scalar('a')], ['a'])).toEqual({ metric: 'a', agg: undefined });
    // Interface-dimensioned sources aggregate too — none is a card candidate today, but the rule
    // has to be "node-level or not", not "entity or not", or a future one silently reads one port.
    expect(resolveCard([entry('a', { dimension: 'interface' })], ['a'])?.agg).toBe('max');
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
    expect(health.cards.vpnTunnels?.agg).toBeUndefined();
    expect(health.cards.sessions).toBeNull();
    expect(health.mem).toBeNull();
  });
});
