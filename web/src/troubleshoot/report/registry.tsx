// SPDX-License-Identifier: AGPL-3.0-only
// The per-tool report registry: which report each Troubleshoot tool gets (1:1 with the scan).
//
// Adding a report = one entry here + one file under `bodies/` + a `reportPath` on the tool in
// `data.ts` + its `report.<tool>.*` i18n keys (EN **and** JA, same commit). `registry.test.ts`
// asserts the biconditional "has a descriptor ⟺ has a reportPath", so a half-wired tool can't ship
// a "View →" button that goes nowhere.

import { AnomalyBody } from './bodies/AnomalyBody';
import { CapacityBody } from './bodies/CapacityBody';
import { CorrelationBody } from './bodies/CorrelationBody';
import { FlapBody } from './bodies/FlapBody';
import {
  correlationDirection,
  detailNum,
  fmtCount,
  maxDetail,
  nodeSummaryStats,
  sevOf,
  sumDetail,
  totalLabel,
} from './format';
import type { ReportDescriptor, ReportRegistry } from './types';

/** Window presets most tools share (the shell resolves the keys with `t()` at render). */
export const WINDOW_PRESETS = {
  h1: { secs: 3_600, labelKey: 'report.common.windows.h1' },
  h6: { secs: 21_600, labelKey: 'report.common.windows.h6' },
  d1: { secs: 86_400, labelKey: 'report.common.windows.d1' },
  d7: { secs: 604_800, labelKey: 'report.common.windows.d7' },
  d30: { secs: 2_592_000, labelKey: 'report.common.windows.d30' },
} as const;

export const BASELINE_PRESETS = {
  d7: { secs: 604_800, labelKey: 'report.common.baselines.d7' },
  d14: { secs: 1_209_600, labelKey: 'report.common.baselines.d14' },
  d30: { secs: 2_592_000, labelKey: 'report.common.baselines.d30' },
} as const;

const anomaly: ReportDescriptor = {
  tool: 'anomaly',
  i18nKey: 'report.anomaly',
  controls: {
    controls: ['scope', 'window', 'baseline', 'sensitivity'],
    windows: [WINDOW_PRESETS.d1, WINDOW_PRESETS.d7, WINDOW_PRESETS.d30],
    baselines: [BASELINE_PRESETS.d14, BASELINE_PRESETS.d30],
    defaults: { windowSecs: 86_400, baselineSecs: 1_209_600, sensitivity: 3, depth: 'standard' },
  },
  summary: nodeSummaryStats(),
  phaseKeys: [
    'report.anomaly.phases.baseline',
    'report.anomaly.phases.fit',
    'report.anomaly.phases.score',
    'report.anomaly.phases.rank',
  ],
  Body: AnomalyBody,
  csv: [
    { header: 'score', cell: (f) => String(Math.round(f.score)) },
    { header: 'severity', cell: (f) => sevOf(f) },
    { header: 'node', cell: (f) => f.node_name },
    { header: 'metric', cell: (f) => f.metric },
    { header: 'shape', cell: (f) => f.kind },
    { header: 'when', cell: (f) => f.when_label },
    { header: 'mean', cell: (f) => String(detailNum(f, 'mean') ?? '') },
    { header: 'sigma', cell: (f) => String(detailNum(f, 'sigma') ?? '') },
  ],
};

/**
 * Correlation counts **pairs**, not nodes — a finding is a relationship between two series, and
 * `node_id` is null on every row, so the standard node stat would always read 0.
 */
const correlation: ReportDescriptor = {
  tool: 'correlation',
  i18nKey: 'report.correlation',
  controls: {
    controls: ['scope', 'window'],
    windows: [WINDOW_PRESETS.h1, WINDOW_PRESETS.h6, WINDOW_PRESETS.d1, WINDOW_PRESETS.d7],
    defaults: { windowSecs: 21_600, baselineSecs: 1_209_600, sensitivity: 3, depth: 'standard' },
  },
  summary: [
    { labelKey: 'report.correlation.summary.pairs', value: totalLabel },
    {
      labelKey: 'report.correlation.summary.strongest',
      value: (f) => {
        const best = f.reduce((m, x) => Math.max(m, Math.abs(detailNum(x, 'r') ?? 0)), 0);
        return best ? best.toFixed(2) : '—';
      },
    },
    {
      labelKey: 'report.correlation.summary.coRising',
      separatorBefore: true,
      value: (f) =>
        String(f.filter((x) => correlationDirection(detailNum(x, 'r') ?? 0) === 'coRising').length),
    },
    {
      labelKey: 'report.correlation.summary.inverse',
      value: (f) =>
        String(f.filter((x) => correlationDirection(detailNum(x, 'r') ?? 0) === 'inverse').length),
    },
  ],
  phaseKeys: ['report.correlation.phases.collect', 'report.correlation.phases.cross', 'report.correlation.phases.rank'],
  Body: CorrelationBody,
  csv: [
    { header: 'score', cell: (f) => String(Math.round(f.score)) },
    { header: 'pair', cell: (f) => f.metric },
    { header: 'r', cell: (f) => String(detailNum(f, 'r') ?? '') },
    { header: 'direction', cell: (f) => correlationDirection(detailNum(f, 'r') ?? 0) },
    { header: 'samples', cell: (f) => String(detailNum(f, 'samples') ?? '') },
  ],
};

const capacity: ReportDescriptor = {
  tool: 'capacity',
  i18nKey: 'report.capacity',
  controls: {
    controls: ['scope', 'window'],
    windows: [WINDOW_PRESETS.d7, WINDOW_PRESETS.d30],
    // The engine floors the window at 7 days regardless — a trend needs history to regress over.
    defaults: { windowSecs: 2_592_000, baselineSecs: 2_592_000, sensitivity: 3, depth: 'standard' },
  },
  summary: [
    { labelKey: 'report.capacity.summary.resources', value: totalLabel },
    {
      labelKey: 'report.capacity.summary.within30',
      tone: 'crit',
      value: (f) => String(f.filter((x) => (detailNum(x, 'tte_days') ?? Infinity) <= 30).length),
    },
    {
      labelKey: 'report.capacity.summary.within90',
      tone: 'warn',
      value: (f) => String(f.filter((x) => (detailNum(x, 'tte_days') ?? Infinity) <= 90).length),
    },
    {
      // The unit ("pp/day") lives in the label so it stays localizable.
      labelKey: 'report.capacity.summary.fastest',
      separatorBefore: true,
      value: (f) => {
        const s = maxDetail(f, 'slope_per_day');
        return s === undefined ? '—' : s.toFixed(2);
      },
    },
  ],
  phaseKeys: ['report.capacity.phases.read', 'report.capacity.phases.project', 'report.capacity.phases.rank'],
  Body: CapacityBody,
  csv: [
    { header: 'score', cell: (f) => String(Math.round(f.score)) },
    { header: 'node', cell: (f) => f.node_name },
    { header: 'metric', cell: (f) => f.metric },
    { header: 'current_pct', cell: (f) => String(detailNum(f, 'current') ?? '') },
    { header: 'slope_per_day', cell: (f) => String(detailNum(f, 'slope_per_day') ?? '') },
    { header: 'days_to_full', cell: (f) => String(detailNum(f, 'tte_days') ?? '') },
  ],
};

const flap: ReportDescriptor = {
  tool: 'flap',
  i18nKey: 'report.flap',
  controls: {
    controls: ['scope', 'window'],
    windows: [WINDOW_PRESETS.d1, WINDOW_PRESETS.d7],
    defaults: { windowSecs: 86_400, baselineSecs: 604_800, sensitivity: 3, depth: 'standard' },
  },
  summary: [
    { labelKey: 'report.flap.summary.nodes', value: totalLabel },
    {
      labelKey: 'report.flap.summary.chronic',
      tone: 'crit',
      value: (f) => String(f.filter((x) => (detailNum(x, 'per_hour') ?? 0) >= 1).length),
    },
    {
      labelKey: 'report.flap.summary.totalFlaps',
      separatorBefore: true,
      value: (f) => fmtCount(sumDetail(f, 'flaps')),
    },
    {
      labelKey: 'report.flap.summary.worstRate',
      value: (f) => {
        const r = maxDetail(f, 'per_hour');
        return r === undefined ? '—' : r.toFixed(1);
      },
    },
  ],
  phaseKeys: ['report.flap.phases.scan', 'report.flap.phases.count', 'report.flap.phases.rank'],
  Body: FlapBody,
  csv: [
    { header: 'score', cell: (f) => String(Math.round(f.score)) },
    { header: 'node', cell: (f) => f.node_name },
    { header: 'flaps', cell: (f) => String(detailNum(f, 'flaps') ?? '') },
    { header: 'per_hour', cell: (f) => String(detailNum(f, 'per_hour') ?? '') },
    { header: 'severity', cell: (f) => sevOf(f) },
  ],
};

export const REPORTS: ReportRegistry = {
  anomaly,
  correlation,
  capacity,
  flap,
};
