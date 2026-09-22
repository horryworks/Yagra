// SPDX-License-Identifier: AGPL-3.0-only
/**
 * What the Overview's Cisco Meraki card decides, kept out of `OverviewTab.tsx` so a test can reach
 * it (`tsxJudgement.test.ts`). The card itself only lays these lines out.
 *
 * ADR-164 増分 13: an MX's WAN uplinks are one row each — the synthetic row key is WAN1 = 1,
 * WAN2 = 2, cellular = 3, and the name arrives with the readings (`row_names`), so the WebUI never
 * spells that mapping a second time.
 */
import type { components } from '../../api/schema';

type RowReading = components['schemas']['MetricRowReading'];

/**
 * The four things `meraki_uplink_status` can say (ADR-164 決定 24). `notConnected` also covers
 * `connecting` and a word the collector did not know — the collector stores all three as 0.
 */
export const MERAKI_UPLINK_STATES = ['active', 'ready', 'notConnected', 'failed'] as const;
export type MerakiUplinkState = (typeof MERAKI_UPLINK_STATES)[number];

/**
 * Read a `meraki_uplink_status` value back into its word.
 *
 * ⚠️ A second copy of `yagra_common::MerakiUplinkStatus::gauge` (Rust): active 2, ready 1,
 * not connected / connecting 0, failed −1 — the encoding the metric's published meaning states.
 * A value that is none of the four (a history point from a future encoding) says nothing rather
 * than something wrong.
 */
export function merakiUplinkState(value: number | null | undefined): MerakiUplinkState | null {
  switch (value) {
    case 2:
      return 'active';
    case 1:
      return 'ready';
    case 0:
      return 'notConnected';
    case -1:
      return 'failed';
    default:
      return null;
  }
}

/** One WAN uplink on the card: its row key, what to call it, its state and its average rates. */
export interface MerakiUplinkLine {
  row: number;
  name: string;
  state: MerakiUplinkState | null;
  /** Average send rate over the traffic collect's interval, bits per second. */
  sentBps: number | null;
  /** Average receive rate over the same window. */
  recvBps: number | null;
}

/**
 * One line per uplink that has any reading, in row order (WAN1, WAN2, cellular).
 *
 * The metrics are joined by row rather than by position: an uplink that reported one of them — or
 * a core that answers the rows of one metric and not another — must not shift every later uplink's
 * number onto its neighbour. A row the Dashboard never named is called by its key, which says less
 * than "WAN2" but never says something false.
 */
export function merakiUplinkLines(
  sent: readonly RowReading[],
  recv: readonly RowReading[],
  status: readonly RowReading[] = [],
): MerakiUplinkLine[] {
  const byRow = new Map<number, MerakiUplinkLine>();
  const line = (r: RowReading): MerakiUplinkLine => {
    let l = byRow.get(r.row);
    if (!l) {
      l = { row: r.row, name: `#${r.row}`, state: null, sentBps: null, recvBps: null };
      byRow.set(r.row, l);
    }
    if (r.name) l.name = r.name;
    return l;
  };
  for (const r of sent) line(r).sentBps = r.value;
  for (const r of recv) line(r).recvBps = r.value;
  for (const r of status) line(r).state = merakiUplinkState(r.value);
  return [...byRow.values()].sort((a, b) => a.row - b.row);
}
