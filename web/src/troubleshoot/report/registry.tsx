// SPDX-License-Identifier: AGPL-3.0-only
// The per-tool report registry: which report each Troubleshoot tool gets (1:1 with the scan).
//
// Adding a report = one entry here + one file under `bodies/` + a `reportPath` on the tool in
// `data.ts` + its `report.<tool>.*` i18n keys (EN **and** JA, same commit). `registry.test.ts`
// asserts the biconditional "has a descriptor ⟺ has a reportPath", so a half-wired tool can't ship
// a "View →" button that goes nowhere.

import { AnomalyBody } from './bodies/AnomalyBody';
import { detailNum, nodeSummaryStats, sevOf } from './format';
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

export const REPORTS: ReportRegistry = {
  anomaly,
};
