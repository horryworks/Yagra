// SPDX-License-Identifier: AGPL-3.0-only
// The Overview tab's numeric derivations, as pure functions.
//
// `metricCards.ts` next door answers *which* gauge a node gets; this answers what its numbers say.
// Both live in `.ts` for the same reason: Vitest here runs `environment: 'node'` with
// `include: ['src/ **\/*.test.ts']`, so arithmetic left in `OverviewTab.tsx` is arithmetic nothing
// runs — and the memory series below is exactly the kind ui-conventions warns about, where a
// mis-paired sample draws a spike the device never had.

import { deriveMem } from '../../lib/format';
import type { MetricPoint } from '../../types/api';
import type { ResolvedMem } from './metricCards';

/** Build the memory usage-% series by aligning a source's two input ranges on shared timestamps
 *  and deriving % per point (`deriveMem`).
 *
 *  The join is on the exact timestamp, and an unmatched point is **dropped** rather than paired
 *  with its neighbour: the two metrics are separate reads, so pairing across timestamps would
 *  invent a percentage from a total and a free that were never observed together. A point whose
 *  pair derives no percentage (a zero or missing total) is dropped for the same reason — the chart
 *  shows a gap, which is the truth, instead of a cliff to 0%. */
export function memPctSeries(
  mem: ResolvedMem,
  byMetric: Record<string, MetricPoint[]>,
): { timestamps: number[]; values: number[] } {
  const [a, b] = mem.metrics;
  const bById = new Map<number, number>();
  for (const p of byMetric[b] ?? []) bById.set(p.t, p.v);
  const timestamps: number[] = [];
  const values: number[] = [];
  for (const p of byMetric[a] ?? []) {
    const vb = bById.get(p.t);
    if (vb == null) continue;
    const { pct } = deriveMem(mem.id, { [a]: p.v, [b]: vb }, mem.unitToBytes);
    if (pct == null) continue;
    timestamps.push(p.t);
    values.push(pct);
  }
  return { timestamps, values };
}

