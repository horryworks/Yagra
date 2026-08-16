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

/** Which of the interface series' optical arrays one line of the optical chart reads. */
export type OpticalSeriesKey = 'rx_power_dbm' | 'tx_power_dbm';

export interface OpticalSeriesSpec {
  key: OpticalSeriesKey;
  /** `nodes`-namespace key for the line's label (and its legend key). */
  labelKey: string;
  /** Slot in `MetricChart`'s `PALETTE`. */
  colorIndex: number;
}

/** The two lines of the optical-power chart, in legend order (ADR-062).
 *
 *  Same shape as [`FAULT_SERIES`] and for the same reason: one object builds both the uPlot series
 *  and the legend swatches, so they cannot disagree. */
export const OPTICAL_SERIES: readonly OpticalSeriesSpec[] = [
  { key: 'rx_power_dbm', labelKey: 'interfaces.opticalRx', colorIndex: 0 },
  { key: 'tx_power_dbm', labelKey: 'interfaces.opticalTx', colorIndex: 1 },
];

/** The values one optical line plots. Missing arrays become empty ones for the same reason as
 *  [`throughputPair`]: a new WebUI can be talking to a core that predates the optical fields, and
 *  `undefined` reaching uPlot takes the whole dock down. */
export function opticalValues(series: InterfaceSeries, spec: OpticalSeriesSpec): (number | null)[] {
  return series[spec.key] ?? [];
}

/** Whether this interface is an optical one — i.e. whether the chart should exist at all.
 *
 *  **This is the whole implementation of "show optical power when the port is optical"** (ADR-062
 *  decision 4). There is deliberately no "is optical" flag on the interface: a reading having
 *  arrived *is* the evidence, and a second source of truth would eventually disagree with the
 *  first. A copper port, a VLAN interface, or a device whose transceiver MIB Yagra does not speak
 *  all produce arrays that are absent or entirely null, and get no chart.
 *
 *  ⚠️ **`null` is not `0` here.** A transceiver reporting exactly 0 dBm (one milliwatt) is a
 *  strong, perfectly normal signal, so emptiness has to be tested by `!= null` rather than by
 *  truthiness — `.some(Boolean)` would hide a healthy port.
 *
 *  ⚠️ **The caller must not ask this until the fetch has settled.** Before the first response the
 *  series is `null` and this answers `false`, which is correct as "nothing to draw yet" and wrong
 *  as "this port has no optics" — rendering an absence as a decision is the bug this comment
 *  exists to prevent. */
export function hasOpticalData(series: InterfaceSeries | null): boolean {
  if (!series) return false;
  return OPTICAL_SERIES.some((spec) => (series[spec.key] ?? []).some((v) => v != null));
}

/** One shaded lane on the optical chart: the window the module says this direction should stay in. */
export interface OpticalBand {
  key: OpticalSeriesKey;
  from: number;
  to: number;
}

/** The transceiver's own acceptable windows, as bands to shade (ADR-062 Inc.4).
 *
 *  A bare dBm figure is unreadable without these — −7 dBm is comfortable on one module and failing
 *  on another — so the band is what turns the chart from a number into an answer. The bounds come
 *  from the module itself, not from anything configured in Yagra, and nothing alerts on them.
 *
 *  **A direction is skipped unless it has both ends.** A one-sided window would shade from a real
 *  bound to the edge of the plot, which reads as "everything above here is fine" — a claim the
 *  module never made. The poller already refuses implausible pairs; this is the second half of the
 *  same rule, applied where the shape (a rectangle needs two sides) makes it unavoidable. */
export function opticalBands(row: {
  rx_power_low_dbm?: number | null;
  rx_power_high_dbm?: number | null;
  tx_power_low_dbm?: number | null;
  tx_power_high_dbm?: number | null;
}): OpticalBand[] {
  const pairs: [OpticalSeriesKey, number | null | undefined, number | null | undefined][] = [
    ['rx_power_dbm', row.rx_power_low_dbm, row.rx_power_high_dbm],
    ['tx_power_dbm', row.tx_power_low_dbm, row.tx_power_high_dbm],
  ];
  const out: OpticalBand[] = [];
  for (const [key, low, high] of pairs) {
    if (low == null || high == null) continue;
    if (!Number.isFinite(low) || !Number.isFinite(high)) continue;
    if (!(low < high)) continue;
    out.push({ key, from: low, to: high });
  }
  return out;
}

/** The window for one direction as a compact range, or `null` when the module reports none.
 *
 *  The shaded band answers "am I inside", this answers "by how much" — and it is the more precise
 *  of the two, because a reader can subtract. It also makes the window legible where a band cannot
 *  be: on a touch device, in a screenshot pasted into a ticket, and to anyone reading the numbers
 *  rather than the picture. Formatting is the caller's (`formatDbm`) so the unit is written once. */
export function opticalWindowText(
  band: OpticalBand | undefined,
  fmt: (v: number) => string,
): string | null {
  if (!band) return null;
  return `${fmt(band.from)} … ${fmt(band.to)}`;
}

/** Y-axis range for the optical chart: the data **and its bands**, with 1 dB of headroom either
 *  side, rounded outward to a whole dB, or `undefined` when there is nothing to bound.
 *
 *  Charts elsewhere let uPlot pick, but dBm needs saying explicitly for two reasons. The values are
 *  **negative**, so an axis anchored at zero would push every real reading into the bottom sliver of
 *  the plot. And the interesting variation is **small** — a link degrading by 3 dB is a link in
 *  trouble — so an auto-range that included zero would flatten exactly the movement worth seeing.
 *
 *  ⚠️ **The bands must be inside the range or they are not drawn at all** — `MetricChart` clamps a
 *  band to the plot, and one entirely below the visible minimum clamps to zero height. That is why
 *  they widen the axis here, and it is a deliberate trade: on a link with 13 dB of margin the line
 *  flattens, because 13 dB of margin *is* the answer. Reading the trend on its own is what the
 *  range control's shorter windows are for.
 *
 *  The floor is not clamped to the plausible window: a reading outside it never reaches the client
 *  (the poller drops it), so anything here is real and should be drawn where it falls. */
export function opticalYRange(
  series: InterfaceSeries | null,
  bands: OpticalBand[] = [],
): [number, number] | undefined {
  if (!series) return undefined;
  let lo = Number.POSITIVE_INFINITY;
  let hi = Number.NEGATIVE_INFINITY;
  for (const spec of OPTICAL_SERIES) {
    for (const v of series[spec.key] ?? []) {
      if (v == null || !Number.isFinite(v)) continue;
      if (v < lo) lo = v;
      if (v > hi) hi = v;
    }
  }
  // Only widen for a band once there is a reading to put beside it: a band alone would draw an
  // axis for a port that has reported nothing, and the chart is not rendered in that state anyway.
  if (Number.isFinite(lo)) {
    for (const b of bands) {
      if (b.from < lo) lo = b.from;
      if (b.to > hi) hi = b.to;
    }
  }
  if (!Number.isFinite(lo) || !Number.isFinite(hi)) return undefined;
  // A dead-flat series would otherwise ask uPlot for a zero-height range.
  return [Math.floor(lo) - 1, Math.ceil(hi) + 1];
}

/** Latest receive level in dBm for the dock's stat tile, or `null` when the port is not optical. */
export function latestRxPower(series: InterfaceSeries | null): number | null {
  return series ? lastNonNull(series.rx_power_dbm) : null;
}

/** Latest transmit level in dBm for the dock's stat tile. Separate from the receive tile rather
 *  than summed: unlike the error and discard pairs these are not two halves of one quantity, and
 *  adding two logarithms would produce a number with no physical meaning. */
export function latestTxPower(series: InterfaceSeries | null): number | null {
  return series ? lastNonNull(series.tx_power_dbm) : null;
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
