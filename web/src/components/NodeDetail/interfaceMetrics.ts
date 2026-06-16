// Pure helpers for the Interfaces tab's Direction-C layout (in-row sparklines + bottom dock).
// Kept framework-free so they're unit-testable in the node test env (the WebUI has no DOM-render
// harness — see vitest.config.ts `environment: 'node'`).

import type { InterfaceSeries } from '../../types/api';

/** Latest combined error rate (in + out, errors/sec) from a fetched interface series, or `null`
 *  when the series is absent or carries no error samples. Used for the dock's "Err" stat tile —
 *  the interface row shape has no error count, so it's derived from the series we already load. */
export function latestErrorRate(series: InterfaceSeries | null): number | null {
  if (!series) return null;
  const inErr = lastNonNull(series.in_errors);
  const outErr = lastNonNull(series.out_errors);
  if (inErr == null && outErr == null) return null;
  return (inErr ?? 0) + (outErr ?? 0);
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
