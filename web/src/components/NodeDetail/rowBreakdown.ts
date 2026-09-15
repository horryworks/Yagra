// SPDX-License-Identifier: AGPL-3.0-only
// Device health, one line per table row (ADR-143).
//
// A switch reports memory per pool and CPU per processor, and the node-wide maximum a card headlines
// cannot say which one. On the Catalyst 2960S this was written for, the memory card read 56% while an
// alert said 83.9%: the card was dividing the largest "used" by the largest "used + free", which on
// that switch happened to be the Processor pool, and the pool at 83.9% — I/O — was on no screen at
// all. These are the pure decisions behind the per-row list: which rows are worth a line, and for
// memory, which pool the headline and the chart follow.
//
// A `.ts` rather than inside the `.tsx`, because Vitest never executes a `.tsx` (testing.md).

import { deriveMem, type MemId } from '../../lib/format';
import type { MetricAgg } from '../../types/api';

/** One row as `GET /nodes/{id}/metrics/{metric}?rows=true` returns it. */
export interface RowValue {
  row: number;
  name?: string | null;
  value: number;
}

/** One memory pool, its two halves joined on the row key. */
export interface MemRow {
  row: number;
  /** The pool's name as the device reports it; `null` when none has been read. */
  name: string | null;
  usedBytes: number;
  totalBytes: number;
  pct: number;
}

/** The rows worth a line, highest first.
 *
 *  A zero row is left out: on a Huawei stack 302 of 306 entity rows are ports, fans and power
 *  supplies with no memory and no CPU, and listing them would bury the four boards that have one.
 *  ⚠️ This is "zero now", the only thing one reading can say — a real CPU idling at exactly 0% is
 *  hidden until it does something, and that is the accepted cost. */
export function visibleRows(rows: readonly RowValue[]): RowValue[] {
  return rows
    .filter((r) => Number.isFinite(r.value) && r.value !== 0)
    .sort((a, b) => b.value - a.value || a.row - b.row);
}

/** Memory pools with both halves joined per row, fullest first.
 *
 *  🚨 **Joined on the row key before dividing** — never the maximum of one half over the maximum of
 *  the other, which is the defect this list replaces. A pool whose total is zero (an ASA reports one
 *  with used 0 and free 0) is not a pool anyone can run out of, and is left out.
 *
 *  `metrics` is the source's pair in its own order — `[used, free]` for Cisco, `[total, free]` for
 *  Huawei — because `deriveMem` reads them by name. */
export function memRows(
  id: MemId,
  metrics: readonly [string, string],
  unitToBytes: number,
  first: readonly RowValue[],
  second: readonly RowValue[],
): MemRow[] {
  const other = new Map(second.map((r) => [r.row, r]));
  const out: MemRow[] = [];
  for (const a of first) {
    const b = other.get(a.row);
    if (!b) continue;
    const d = deriveMem(id, { [metrics[0]]: a.value, [metrics[1]]: b.value }, unitToBytes);
    if (d.usedBytes == null || d.totalBytes == null || d.pct == null || !(d.totalBytes > 0)) {
      continue;
    }
    out.push({
      row: a.row,
      name: a.name ?? b.name ?? null,
      usedBytes: d.usedBytes,
      totalBytes: d.totalBytes,
      pct: d.pct,
    });
  }
  return out.sort((x, y) => y.pct - x.pct || x.row - y.row);
}

/** The row a chart should follow, or `null` for the node-wide series.
 *
 *  Only when there is more than one row. A scalar source (a Net-SNMP host's memory) reads back as
 *  row `0` from a series that carries no row label, so asking for row 0's history would ask for a
 *  series that does not exist — and with one row, its history *is* the node's. */
export function followedRow<T extends { row: number }>(rows: readonly T[]): T | null {
  return rows.length > 1 ? rows[0] : null;
}

/** What the memory card headlines: the fullest pool, joined on its own row — or, when there are no
 *  rows (a core that does not return them), the node-wide reading, the only answer there is.
 *
 *  🚨 Never the node-wide reading while there are rows. It divides the largest "used" by the largest
 *  "used + free", which on a C2960S read 56% while the I/O pool sat at 83.9% under an alert — so it
 *  is not even computed when a pool is available. `rows` is `memRows`' answer, fullest first. */
export function memHeadline<T>(rows: readonly MemRow[], nodeWide: () => T): MemRow | T {
  return rows.length > 0 ? rows[0] : nodeWide();
}

/** How the memory chart reads its two series: the followed pool's own row when there are several,
 *  the node-wide maximum otherwise — `followedRow` says why one row is read node-wide. */
export function memRangeQuery(rows: readonly MemRow[]): { row: number } | { agg: MetricAgg } {
  const followed = followedRow(rows);
  return followed ? { row: followed.row } : { agg: 'max' };
}
