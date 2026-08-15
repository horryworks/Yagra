// SPDX-License-Identifier: AGPL-3.0-only
// Pure helpers for the Interfaces tab's Direction-C layout (in-row sparklines + bottom dock).
// Kept framework-free so they're unit-testable in the node test env (the WebUI has no DOM-render
// harness — see vitest.config.ts `environment: 'node'`).

import { formatBps } from '../../lib/format';
import type { InterfaceSeries } from '../../types/api';
import type { ThroughputScale } from '../../prefs';

/** Throughput-chart bandwidth overlay derived from an interface's configured speed and the global
 *  Y-axis mode. Returns the red reference line (configured bandwidth) and, in `capacity` mode, a
 *  fixed `[0, bandwidth]` Y range. A non-positive/absent speed (interfaces with no concept of
 *  bandwidth) yields neither — the caller then shows no line and no toggle. */
export function throughputBandwidthOverlay(
  ifSpeedBps: number | null | undefined,
  mode: ThroughputScale,
): { referenceLine?: { value: number; label: string }; yRange?: [number, number] } {
  if (ifSpeedBps == null || !(ifSpeedBps > 0)) return {};
  return {
    referenceLine: { value: ifSpeedBps, label: formatBps(ifSpeedBps) },
    yRange: mode === 'capacity' ? [0, ifSpeedBps] : undefined,
  };
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
