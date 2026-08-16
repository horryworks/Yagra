// SPDX-License-Identifier: AGPL-3.0-only
// Pure helpers for the Interfaces tab's Direction-C layout (in-row sparklines + bottom dock).
// Kept framework-free so they're unit-testable in the node test env (the WebUI has no DOM-render
// harness — see vitest.config.ts `environment: 'node'`).

import { formatBps } from '../../lib/format';
import type { InterfaceSeries } from '../../types/api';
import type { RateUnit, ThroughputScale } from '../../prefs';

/** Throughput-chart bandwidth overlay derived from an interface's configured speed, the global
 *  Y-axis mode and the plotted unit. Returns the red reference line (configured bandwidth) and, in
 *  `capacity` mode, a fixed `[0, bandwidth]` Y range. A non-positive/absent speed (interfaces with
 *  no concept of bandwidth) yields neither — the caller then shows no line and no toggle.
 *
 *  ⚠️ **`unit: 'pps'` also yields neither, and that is the point of taking the unit at all**
 *  (ADR-060). `ifSpeed` is bits per second, so on a packets-per-second axis the line would land at
 *  an arbitrary height and read as a capacity the operator is nowhere near — worse than no line,
 *  because it looks like an answer. The capacity Y-range goes with it for the same reason.
 *
 *  This lives here rather than in the JSX because Vitest runs `environment: 'node'` and never
 *  executes `.tsx`, so a rule written at the call site is a rule no test can reach. */
export function throughputBandwidthOverlay(
  ifSpeedBps: number | null | undefined,
  mode: ThroughputScale,
  unit: RateUnit,
): { referenceLine?: { value: number; label: string }; yRange?: [number, number] } {
  if (unit === 'pps') return {};
  if (ifSpeedBps == null || !(ifSpeedBps > 0)) return {};
  return {
    referenceLine: { value: ifSpeedBps, label: formatBps(ifSpeedBps) },
    yRange: mode === 'capacity' ? [0, ifSpeedBps] : undefined,
  };
}

/** The directional pair of the interface series the throughput chart plots for `unit`
 *  (ADR-060). Parameterized here rather than branched in the component for the same reason as
 *  [`throughputBandwidthOverlay`]: these four arrays are the same type, so picking the wrong pair
 *  is a silent mislabelling — a pps axis drawn from bps values — that nothing would catch.
 *
 *  Missing arrays become empty ones. The generated type says they are always present, but that is
 *  a statement about *this* core: web and core are separate containers, so during a rolling upgrade
 *  a new WebUI can be talking to a core that predates the pps fields. An empty series draws nothing
 *  and the chart says "no data"; `undefined` reaching uPlot would throw and take the dock down. */
export function throughputPair(
  series: InterfaceSeries,
  unit: RateUnit,
): [(number | null)[], (number | null)[]] {
  return unit === 'pps'
    ? [series.in_ucast_pps ?? [], series.out_ucast_pps ?? []]
    : [series.in_bps ?? [], series.out_bps ?? []];
}

/** Which of the interface series' fault arrays one line of the errors/discards chart reads. */
export type FaultSeriesKey = 'in_errors' | 'out_errors' | 'in_discards' | 'out_discards';

export interface FaultSeriesSpec {
  key: FaultSeriesKey;
  /** `nodes`-namespace key for the line's label (and its legend key). */
  labelKey: string;
  /** Slot in `MetricChart`'s `PALETTE`. Every line gets its own hue: the in/out colour pair the
   *  throughput chart uses only carries two meanings, and this chart carries four. */
  colorIndex: number;
}

/** The four lines of the combined errors/discards chart, in legend order.
 *
 *  Errors and discards shared a unit from the start and now share a chart. ⚠️ **They deliberately
 *  did not** (ADR-046 Inc.4 decision A): the two have different causes — damage vs congestion — and
 *  in practice differ by orders of magnitude, so one linear axis flattens whichever is smaller.
 *  That cost is real and unchanged; what changed is the judgement, on the grounds that both are
 *  zero on a healthy link and a dock of two charts reads better than one of three (ADR-046 Inc.5).
 *
 *  The list lives here, rather than inline in the chart, so the mapping from an array to a colour
 *  and a label is one object that both the uPlot series and the legend swatches are built from —
 *  a legend that disagrees with its chart is a wrong answer that looks like an answer. */
export const FAULT_SERIES: readonly FaultSeriesSpec[] = [
  { key: 'in_errors', labelKey: 'interfaces.errIn', colorIndex: 0 },
  { key: 'out_errors', labelKey: 'interfaces.errOut', colorIndex: 1 },
  { key: 'in_discards', labelKey: 'interfaces.discIn', colorIndex: 2 },
  { key: 'out_discards', labelKey: 'interfaces.discOut', colorIndex: 3 },
];

/** The values one fault line plots. Missing arrays become empty ones for the same reason as
 *  [`throughputPair`]: web and core are separate containers, so a new WebUI can be talking to an
 *  older core, and `undefined` reaching uPlot takes the whole dock down. */
export function faultValues(series: InterfaceSeries, spec: FaultSeriesSpec): (number | null)[] {
  return series[spec.key] ?? [];
}

/** Latest combined rate (in + out, per second) across a directional pair of the interface series,
 *  or `null` when the series is absent or neither direction carries a sample. Parameterized rather
 *  than duplicated per pair: errors and discards differ only in which two arrays they read, and a
 *  copy would be a second place to fix the "both directions absent ⇒ null, one absent ⇒ 0" rule. */
function latestPairRate(
  series: InterfaceSeries | null,
  pick: (s: InterfaceSeries) => [(number | null)[] | undefined, (number | null)[] | undefined],
): number | null {
  if (!series) return null;
  const [inArr, outArr] = pick(series);
  const inVal = lastNonNull(inArr);
  const outVal = lastNonNull(outArr);
  if (inVal == null && outVal == null) return null;
  return (inVal ?? 0) + (outVal ?? 0);
}

/** Latest combined error rate (in + out, errors/sec). Used for the dock's "Err" stat tile — the
 *  interface row shape has no error count, so it's derived from the series we already load. */
export function latestErrorRate(series: InterfaceSeries | null): number | null {
  return latestPairRate(series, (s) => [s.in_errors, s.out_errors]);
}

/** Latest combined discard rate (in + out, discards/sec) for the dock's "Disc" stat tile.
 *  Deliberately separate from the error rate: a discard is congestion, an error is damage, and
 *  summing them would tell the operator to check the wrong thing (ADR-046 Inc.4). */
export function latestDiscardRate(series: InterfaceSeries | null): number | null {
  return latestPairRate(series, (s) => [s.in_discards, s.out_discards]);
}

/** Last non-null value of a gapped series array, or `null` when it's empty / all gaps. */
function lastNonNull(arr: (number | null)[] | undefined): number | null {
  if (!arr) return null;
  for (let i = arr.length - 1; i >= 0; i -= 1) {
    const v = arr[i];
    if (v != null) return v;
  }
  return null;
}

/** SVG path data for an in-row throughput sparkline: a line through `values` plus a closed area
 *  under it, scaled to the series' own peak (×1.1 headroom) within a `width`×`height` box with a
 *  `pad` inset. Returns `null` for fewer than two points (nothing to draw). No axes/interaction —
 *  these are cheap at-a-glance trend marks, one per interface row. */
export function sparklinePath(
  values: number[],
  width: number,
  height: number,
  pad = 2,
): { line: string; area: string } | null {
  const n = values.length;
  if (n < 2) return null;
  const hi = Math.max(...values, 1) * 1.1;
  const innerW = width - pad * 2;
  const innerH = height - pad * 2;
  const xa = (i: number) => pad + (i / (n - 1)) * innerW;
  const ya = (v: number) => pad + innerH - (Math.max(0, v) / hi) * innerH;
  let line = '';
  values.forEach((v, i) => {
    line += `${i ? 'L' : 'M'}${xa(i).toFixed(1)} ${ya(v).toFixed(1)} `;
  });
  line = line.trim();
  const baseline = (height - pad).toFixed(1);
  const area = `${line} L${xa(n - 1).toFixed(1)} ${baseline} L${xa(0).toFixed(1)} ${baseline} Z`;
  return { line, area };
}
