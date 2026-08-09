// SPDX-License-Identifier: AGPL-3.0-only
// The widget's failure mode is not an error — it is a ranked list that reads like a measurement and
// is not one. Ranking a raw counter puts the longest-uptime node on top and calls it the busiest;
// suggesting a counter is the same mistake one step earlier. Both are decided here.

import { describe, expect, it } from 'vitest';
import { METRIC_PRESETS } from '../../lib/suppression';
import { METRIC_KINDS } from '../../types/api';
import type { MetricKind } from '../../types/api';
import { metricSuggestions, metricTopPlan, readTopSelection } from './metricTop';

const known = (pairs: Record<string, MetricKind>): ReadonlyMap<string, MetricKind> =>
  new Map(Object.entries(pairs));

describe('readTopSelection', () => {
  it('reads the metric and the window', () => {
    expect(readTopSelection({ metric: 'huawei_cpu_usage', agg: 'max_1h' })).toEqual({
      metric: 'huawei_cpu_usage',
      agg: 'max_1h',
    });
  });

  it('defaults a fresh widget to nothing picked, ranked now', () => {
    expect(readTopSelection(undefined)).toEqual({ metric: null, agg: 'now' });
    expect(readTopSelection({})).toEqual({ metric: null, agg: 'now' });
  });

  it('refuses a non-string metric and an unrecognised window', () => {
    // The layout document is user-editable JSON that round-trips through storage; neither field may
    // reach the query as whatever happens to be in the bag.
    expect(readTopSelection({ metric: 42, agg: 'yesterday' })).toEqual({ metric: null, agg: 'now' });
  });

  it('trims the typed name', () => {
    // The edge validates the metric as an identifier, so a stray leading space is refused with
    // `invalid_metric_name` — an error about something the operator cannot see.
    expect(readTopSelection({ metric: '  icmp_loss_pct ' }).metric).toBe('icmp_loss_pct');
    expect(readTopSelection({ metric: '   ' }).metric).toBeNull();
  });
});

describe('metricSuggestions', () => {
  it('always offers the shared presets, even before anything has been seen', () => {
    // A dashboard is often the first page loaded, so the memory is empty exactly when a new widget
    // is being configured. An empty list would read as "there is nothing to rank".
    expect(metricSuggestions(new Map())).toEqual([...METRIC_PRESETS].sort());
  });

  it('adds what the session has seen, sorted and deduplicated', () => {
    const s = metricSuggestions(known({ juniper_temp_c: 'gauge', icmp_rtt_ms: 'gauge' }));
    expect(s).toContain('juniper_temp_c');
    expect(s.filter((n) => n === 'icmp_rtt_ms')).toHaveLength(1);
    expect(s).toEqual([...s].sort());
  });

  it('never suggests a counter', () => {
    const s = metricSuggestions(known({ if_hc_in_octets: 'counter', huawei_cpu_usage: 'gauge' }));
    expect(s).not.toContain('if_hc_in_octets');
    expect(s).toContain('huawei_cpu_usage');
  });

  it('drops a preset that turns out to be a counter', () => {
    // The presets are a hand-maintained list in another module; a real sighting outranks it rather
    // than the two disagreeing silently.
    expect(metricSuggestions(known({ [METRIC_PRESETS[0]]: 'counter' }))).not.toContain(
      METRIC_PRESETS[0],
    );
  });
});

describe('metricTopPlan', () => {
  it('asks for a metric before ranking anything', () => {
    expect(metricTopPlan({ metric: null, agg: 'now' }, new Map())).toEqual({ kind: 'pick-metric' });
  });

  it('ranks a gauge over the chosen window', () => {
    expect(
      metricTopPlan({ metric: 'huawei_cpu_usage', agg: 'max_1h' }, known({ huawei_cpu_usage: 'gauge' })),
    ).toEqual({ kind: 'rank', metric: 'huawei_cpu_usage', agg: 'max_1h' });
  });

  it('refuses a metric it has seen as a counter', () => {
    // `/metrics/top` ranks stored values and has no rate mode, so this would rank odometer
    // readings — the ADR-012 accident, on the ranking surface.
    expect(metricTopPlan({ metric: 'if_in_errors', agg: 'now' }, known({ if_in_errors: 'counter' }))).toEqual(
      { kind: 'counter', metric: 'if_in_errors' },
    );
  });

  it('ranks a name it has never seen rather than refusing it', () => {
    // The memory is what this browser happened to look at, not a catalogue. Treating "not seen" as
    // "does not exist" would make the widget useless on a fresh load, which is when it is added.
    expect(metricTopPlan({ metric: 'acme_widget_temp', agg: 'now' }, new Map())).toEqual({
      kind: 'rank',
      metric: 'acme_widget_temp',
      agg: 'now',
    });
  });

  it('only ever plans to rank a metric the picker would have offered', () => {
    // The biconditional: whatever `metricSuggestions` shows must be rankable, and anything it hides
    // for being a counter must be refused. Drift between the two is how a counter gets ranked.
    for (const kind of METRIC_KINDS as readonly MetricKind[]) {
      const k = known({ probe_metric: kind });
      const offered = metricSuggestions(k).includes('probe_metric');
      const plan = metricTopPlan({ metric: 'probe_metric', agg: 'now' }, k);
      expect(plan.kind === 'rank').toBe(offered);
    }
  });
});
