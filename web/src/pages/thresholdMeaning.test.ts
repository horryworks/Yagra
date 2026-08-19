// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { EXPLAINED_METRICS, metricMeaningKey } from './thresholdMeaning';
import { LIVENESS_METRIC } from '../lib/format';
import { METRIC_PRESETS } from '../lib/suppression';

describe('metricMeaningKey', () => {
  it('answers for every metric it claims to explain', () => {
    for (const m of EXPLAINED_METRICS) {
      expect(metricMeaningKey(m)).toBe(`thresholds.meaning.${m}`);
    }
  });

  it('answers null for a metric it does not explain', () => {
    // The rejecting side, and it needs the receiving side above to mean anything: a lookup that
    // answered for everything would fill the column with keys that resolve to nothing, and one
    // that answered for nothing would be indistinguishable from "no metric is explained yet".
    expect(metricMeaningKey('huawei_temp')).toBeNull();
    expect(metricMeaningKey('')).toBeNull();
    // The near miss that makes the boundary real: `snmp_sys_uptime_ticks` is offered as a preset
    // and sits in five built-in templates, so the Metric set that collects it is where it is
    // explained. If this starts returning a key, the rule in the module doc has quietly changed.
    expect(metricMeaningKey('snmp_sys_uptime_ticks')).toBeNull();
  });

  it('explains the reachability sentinel, the row that reads worst without it', () => {
    // `Reachability | below | (no bounds) | 3 breaches` is the row this whole column exists for.
    expect(metricMeaningKey(LIVENESS_METRIC)).toBe(`thresholds.meaning.${LIVENESS_METRIC}`);
  });

  it('explains every offered preset except the one with a Metric set entry', () => {
    // The presets are what the add-rule form suggests, so they are the names an operator is most
    // likely to meet bare. Pinned as a relation rather than a copied list, so extending either
    // side forces the question rather than silently leaving a preset unexplained.
    expect(METRIC_PRESETS.filter((m) => metricMeaningKey(m) === null)).toEqual([
      'snmp_sys_uptime_ticks',
    ]);
  });

  it('lists no metric twice', () => {
    // The array is iterated by the i18n coverage test; a duplicate would pass every check while
    // meaning someone edited the list without reading it.
    expect(new Set(EXPLAINED_METRICS).size).toBe(EXPLAINED_METRICS.length);
  });
});
