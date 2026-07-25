// SPDX-License-Identifier: AGPL-3.0-only
// The per-tool report registry: which report each Troubleshoot tool gets (1:1 with the scan).
//
// Adding a report = one entry here + one file under `bodies/` + a `reportPath` on the tool in
// `data.ts` + its `report.<tool>.*` i18n keys (EN **and** JA, same commit). `registry.test.ts`
// asserts the biconditional "has a descriptor ⟺ has a reportPath", so a half-wired tool can't ship
// a "View →" button that goes nowhere.

import { formatBytes } from '../../lib/format';
import { AnomalyBody } from './bodies/AnomalyBody';
import { AuthProbeBody } from './bodies/AuthProbeBody';
import { CapacityBody } from './bodies/CapacityBody';
import { CorrelationBody } from './bodies/CorrelationBody';
import { EventFlapBody } from './bodies/EventFlapBody';
import { EventStormBody } from './bodies/EventStormBody';
import { FlapBody } from './bodies/FlapBody';
import { RuleGapBody } from './bodies/RuleGapBody';
import { SeverityShiftBody } from './bodies/SeverityShiftBody';
import { TalkerShiftBody } from './bodies/TalkerShiftBody';
import { TrafficAnomalyBody } from './bodies/TrafficAnomalyBody';
import {
  correlationDirection,
  countDetailValues,
  countNodes,
  detailNum,
  detailStr,
  eventRuleName,
  fmtCount,
  groupByRule,
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

const eventStorm: ReportDescriptor = {
  tool: 'event_storm',
  i18nKey: 'report.event_storm',
  controls: {
    controls: ['scope', 'window', 'baseline', 'sensitivity'],
    windows: [WINDOW_PRESETS.h1, WINDOW_PRESETS.h6, WINDOW_PRESETS.d1],
    baselines: [BASELINE_PRESETS.d7, BASELINE_PRESETS.d14],
    defaults: { windowSecs: 3_600, baselineSecs: 604_800, sensitivity: 3, depth: 'standard' },
  },
  summary: [
    { labelKey: 'report.event_storm.summary.nodes', value: totalLabel },
    {
      labelKey: 'report.event_storm.summary.peak',
      value: (f) => {
        const p = maxDetail(f, 'peak');
        return p === undefined ? '—' : fmtCount(Math.round(p));
      },
    },
    {
      labelKey: 'report.event_storm.summary.worstRatio',
      separatorBefore: true,
      value: (f) => {
        // Nodes with no baseline at all produce an infinite ratio — report the finite worst instead
        // of rendering "×∞".
        const ratios = f
          .map((x) => {
            const b = detailNum(x, 'baseline_mean') ?? 0;
            return b > 0 ? (detailNum(x, 'peak') ?? 0) / b : NaN;
          })
          .filter((r) => Number.isFinite(r));
        return ratios.length ? `×${Math.max(...ratios).toFixed(1)}` : '—';
      },
    },
  ],
  phaseKeys: ['report.event_storm.phases.read', 'report.event_storm.phases.score'],
  Body: EventStormBody,
  csv: [
    { header: 'score', cell: (f) => String(Math.round(f.score)) },
    { header: 'node', cell: (f) => f.node_name },
    { header: 'peak_events', cell: (f) => String(detailNum(f, 'peak') ?? '') },
    { header: 'baseline_mean', cell: (f) => String(detailNum(f, 'baseline_mean') ?? '') },
    { header: 'bucket_secs', cell: (f) => String(detailNum(f, 'bucket_secs') ?? '') },
    { header: 'peak_at_unix', cell: (f) => String(detailNum(f, 'peak_at') ?? '') },
  ],
};

const severityShift: ReportDescriptor = {
  tool: 'severity_shift',
  i18nKey: 'report.severity_shift',
  controls: {
    controls: ['scope', 'window', 'baseline'],
    windows: [WINDOW_PRESETS.h1, WINDOW_PRESETS.h6, WINDOW_PRESETS.d1],
    baselines: [BASELINE_PRESETS.d7, BASELINE_PRESETS.d14],
    defaults: { windowSecs: 21_600, baselineSecs: 604_800, sensitivity: 3, depth: 'standard' },
  },
  summary: [
    { labelKey: 'report.severity_shift.summary.nodes', value: totalLabel },
    {
      labelKey: 'report.severity_shift.summary.biggest',
      value: (f) => {
        if (!f.length) return '—';
        const pp = Math.max(
          ...f.map(
            (x) => ((detailNum(x, 'recent_high_frac') ?? 0) - (detailNum(x, 'baseline_high_frac') ?? 0)) * 100,
          ),
        );
        return `${pp.toFixed(0)}`;
      },
    },
    {
      labelKey: 'report.severity_shift.summary.highEvents',
      separatorBefore: true,
      value: (f) => fmtCount(sumDetail(f, 'recent_high')),
    },
    {
      labelKey: 'report.severity_shift.summary.examined',
      value: (f) => fmtCount(sumDetail(f, 'recent_total')),
    },
  ],
  phaseKeys: ['report.severity_shift.phases.read', 'report.severity_shift.phases.compare'],
  Body: SeverityShiftBody,
  csv: [
    { header: 'score', cell: (f) => String(Math.round(f.score)) },
    { header: 'node', cell: (f) => f.node_name },
    { header: 'baseline_high_frac', cell: (f) => String(detailNum(f, 'baseline_high_frac') ?? '') },
    { header: 'recent_high_frac', cell: (f) => String(detailNum(f, 'recent_high_frac') ?? '') },
    { header: 'recent_high', cell: (f) => String(detailNum(f, 'recent_high') ?? '') },
    { header: 'recent_total', cell: (f) => String(detailNum(f, 'recent_total') ?? '') },
  ],
};

/**
 * Rule gap counts **signatures**, not nodes — findings are cross-node by nature (`node_id` is often
 * null with `node_name: "fleet"`), so a node stat would under-report.
 */
const ruleGap: ReportDescriptor = {
  tool: 'rule_gap',
  i18nKey: 'report.rule_gap',
  controls: {
    controls: ['scope', 'window'],
    windows: [WINDOW_PRESETS.d1, WINDOW_PRESETS.d7],
    defaults: { windowSecs: 86_400, baselineSecs: 604_800, sensitivity: 3, depth: 'standard' },
  },
  summary: [
    { labelKey: 'report.rule_gap.summary.signatures', value: totalLabel },
    {
      labelKey: 'report.rule_gap.summary.events',
      value: (f) => fmtCount(sumDetail(f, 'count')),
    },
    {
      labelKey: 'report.rule_gap.summary.sources',
      separatorBefore: true,
      value: (f) => String(countDetailValues(f, 'kind')),
    },
  ],
  phaseKeys: ['report.rule_gap.phases.cluster', 'report.rule_gap.phases.rank'],
  Body: RuleGapBody,
  csv: [
    { header: 'score', cell: (f) => String(Math.round(f.score)) },
    { header: 'source_kind', cell: (f) => detailStr(f, 'kind') ?? '' },
    { header: 'signature', cell: (f) => detailStr(f, 'signature') ?? f.metric },
    { header: 'events', cell: (f) => String(detailNum(f, 'count') ?? '') },
    { header: 'sample_node', cell: (f) => f.node_name },
  ],
};

/** Auth probe counts **sources** — the node is the target, not the entity. */
const authProbe: ReportDescriptor = {
  tool: 'auth_probe',
  i18nKey: 'report.auth_probe',
  controls: {
    controls: ['scope', 'window'],
    windows: [WINDOW_PRESETS.h1, WINDOW_PRESETS.h6, WINDOW_PRESETS.d1],
    defaults: { windowSecs: 21_600, baselineSecs: 604_800, sensitivity: 3, depth: 'standard' },
  },
  summary: [
    { labelKey: 'report.auth_probe.summary.sources', value: totalLabel },
    {
      labelKey: 'report.auth_probe.summary.failures',
      value: (f) => fmtCount(sumDetail(f, 'count')),
    },
    {
      labelKey: 'report.auth_probe.summary.worst',
      value: (f) => {
        const m = maxDetail(f, 'count');
        return m === undefined ? '—' : fmtCount(m);
      },
    },
    {
      labelKey: 'report.auth_probe.summary.targets',
      separatorBefore: true,
      value: (f) => String(countNodes(f)),
    },
  ],
  phaseKeys: ['report.auth_probe.phases.cluster', 'report.auth_probe.phases.rank'],
  Body: AuthProbeBody,
  csv: [
    { header: 'score', cell: (f) => String(Math.round(f.score)) },
    { header: 'source_ip', cell: (f) => detailStr(f, 'source_ip') ?? '' },
    { header: 'failures', cell: (f) => String(detailNum(f, 'count') ?? '') },
    { header: 'target_node', cell: (f) => f.node_name },
    { header: 'severity', cell: (f) => sevOf(f) },
  ],
};

/** Event flap's entity is a (rule, node) pair, so the strip counts pairs AND distinct rules. */
const eventFlap: ReportDescriptor = {
  tool: 'event_flap',
  i18nKey: 'report.event_flap',
  controls: {
    controls: ['scope', 'window'],
    windows: [WINDOW_PRESETS.h6, WINDOW_PRESETS.d1, WINDOW_PRESETS.d7],
    defaults: { windowSecs: 21_600, baselineSecs: 604_800, sensitivity: 3, depth: 'standard' },
  },
  summary: [
    { labelKey: 'report.event_flap.summary.pairs', value: totalLabel },
    { labelKey: 'report.event_flap.summary.rules', value: (f) => String(groupByRule(f).length) },
    {
      labelKey: 'report.event_flap.summary.nodes',
      value: (f) => String(countNodes(f)),
    },
    {
      labelKey: 'report.event_flap.summary.worstRate',
      separatorBefore: true,
      value: (f) => {
        const r = maxDetail(f, 'per_hour');
        return r === undefined ? '—' : r.toFixed(1);
      },
    },
  ],
  phaseKeys: ['report.event_flap.phases.read', 'report.event_flap.phases.rank'],
  Body: EventFlapBody,
  csv: [
    { header: 'score', cell: (f) => String(Math.round(f.score)) },
    { header: 'node', cell: (f) => f.node_name },
    { header: 'rule', cell: (f) => eventRuleName(f.metric) },
    { header: 'fires', cell: (f) => String(detailNum(f, 'fires') ?? '') },
    { header: 'clears', cell: (f) => String(detailNum(f, 'clears') ?? '') },
    { header: 'cycles', cell: (f) => String(detailNum(f, 'cycles') ?? '') },
    { header: 'per_hour', cell: (f) => String(detailNum(f, 'per_hour') ?? '') },
  ],
};

const trafficAnomaly: ReportDescriptor = {
  tool: 'traffic_anomaly',
  i18nKey: 'report.traffic_anomaly',
  controls: {
    controls: ['scope', 'window', 'baseline', 'sensitivity'],
    windows: [WINDOW_PRESETS.h1, WINDOW_PRESETS.h6, WINDOW_PRESETS.d1],
    baselines: [BASELINE_PRESETS.d7, BASELINE_PRESETS.d14],
    defaults: { windowSecs: 3_600, baselineSecs: 604_800, sensitivity: 3, depth: 'standard' },
  },
  summary: [
    { labelKey: 'report.traffic_anomaly.summary.nodes', value: totalLabel },
    {
      labelKey: 'report.traffic_anomaly.summary.peak',
      value: (f) => {
        const p = maxDetail(f, 'peak_bytes');
        return p === undefined ? '—' : formatBytes(p);
      },
    },
    {
      labelKey: 'report.traffic_anomaly.summary.worstRatio',
      separatorBefore: true,
      value: (f) => {
        const ratios = f
          .map((x) => {
            const b = detailNum(x, 'baseline_mean_bytes') ?? 0;
            return b > 0 ? (detailNum(x, 'peak_bytes') ?? 0) / b : NaN;
          })
          .filter((r) => Number.isFinite(r));
        return ratios.length ? `×${Math.max(...ratios).toFixed(1)}` : '—';
      },
    },
  ],
  phaseKeys: ['report.traffic_anomaly.phases.read', 'report.traffic_anomaly.phases.score'],
  Body: TrafficAnomalyBody,
  csv: [
    { header: 'score', cell: (f) => String(Math.round(f.score)) },
    { header: 'node', cell: (f) => f.node_name },
    { header: 'peak_bytes', cell: (f) => String(detailNum(f, 'peak_bytes') ?? '') },
    { header: 'baseline_mean_bytes', cell: (f) => String(detailNum(f, 'baseline_mean_bytes') ?? '') },
    { header: 'peak_at_unix', cell: (f) => String(detailNum(f, 'peak_at') ?? '') },
  ],
};

/** Talker shift counts **addresses** — the node is where the talker appeared, not the entity. */
const talkerShift: ReportDescriptor = {
  tool: 'talker_shift',
  i18nKey: 'report.talker_shift',
  controls: {
    controls: ['scope', 'window'],
    windows: [WINDOW_PRESETS.h1, WINDOW_PRESETS.h6, WINDOW_PRESETS.d1],
    defaults: { windowSecs: 3_600, baselineSecs: 604_800, sensitivity: 3, depth: 'standard' },
  },
  summary: [
    { labelKey: 'report.talker_shift.summary.talkers', value: totalLabel },
    {
      labelKey: 'report.talker_shift.summary.bytes',
      value: (f) => formatBytes(sumDetail(f, 'bytes')),
    },
    {
      labelKey: 'report.talker_shift.summary.bestRank',
      value: (f) => {
        // "Best" is the LOWEST rank number — a new #1 is the strongest signal.
        const ranks = f.map((x) => detailNum(x, 'rank')).filter((r): r is number => r !== undefined);
        return ranks.length ? `#${Math.min(...ranks)}` : '—';
      },
    },
    {
      labelKey: 'report.talker_shift.summary.nodes',
      separatorBefore: true,
      value: (f) => String(countNodes(f)),
    },
  ],
  phaseKeys: ['report.talker_shift.phases.compare', 'report.talker_shift.phases.rank'],
  Body: TalkerShiftBody,
  csv: [
    { header: 'score', cell: (f) => String(Math.round(f.score)) },
    { header: 'address', cell: (f) => detailStr(f, 'addr') ?? '' },
    { header: 'node', cell: (f) => f.node_name },
    { header: 'bytes', cell: (f) => String(detailNum(f, 'bytes') ?? '') },
    { header: 'new_rank', cell: (f) => String(detailNum(f, 'rank') ?? '') },
  ],
};

export const REPORTS: ReportRegistry = {
  anomaly,
  correlation,
  capacity,
  flap,
  event_storm: eventStorm,
  event_flap: eventFlap,
  severity_shift: severityShift,
  rule_gap: ruleGap,
  auth_probe: authProbe,
  traffic_anomaly: trafficAnomaly,
  talker_shift: talkerShift,
};
