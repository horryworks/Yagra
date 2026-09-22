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
import type { MerakiHaRole, MerakiPair, MerakiPairState } from '../../types/api';

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

/** The card's Auto VPN line (ADR-164 決定 25), or `null` when there is nothing to say. */
export interface MerakiVpnLine {
  /** Hubs this MX reaches, and how many were counted. */
  reached: number;
  total: number;
  /** On a hub: spokes it does not reach. `null` on a spoke. */
  spokesDown: number | null;
  /** `critical` when no counted hub is reached, `warning` when some are not, else `ok` — the same
   *  two lines the seeded rule draws on the share. */
  tone: 'ok' | 'warning' | 'critical';
}

/**
 * What the VPN line says, from the three node-level readings. The collector reports hubs only when
 * it counted at least one — an MX that is down, or whose only hub is down, has no reading — so no
 * reading draws no line rather than a guessed "fine".
 */
export function merakiVpnLine(
  hubsReachable: number | null,
  hubsUnreachable: number | null,
  spokesUnreachable: number | null,
): MerakiVpnLine | null {
  if (hubsReachable == null || hubsUnreachable == null) {
    return spokesUnreachable == null
      ? null
      : { reached: 0, total: 0, spokesDown: spokesUnreachable, tone: 'ok' };
  }
  const total = hubsReachable + hubsUnreachable;
  const tone = hubsUnreachable === 0 ? 'ok' : hubsReachable === 0 ? 'critical' : 'warning';
  return { reached: hubsReachable, total, spokesDown: spokesUnreachable, tone };
}

/** The card's warm-spare line (ADR-164 決定 26). */
export interface MerakiPairLine {
  role: MerakiHaRole;
  state: MerakiPairState;
  /** The other MX, when the server named one the caller may see. */
  partner: { name: string; role: MerakiHaRole | null; nodeId: string | null } | null;
  /** The colour the state is drawn in; `null` draws it uncoloured (`unknown` says nothing). */
  tone: 'ok' | 'warning' | 'critical' | null;
  /**
   * Why the card has no VPN line, when that is the pair's doing. Meraki reports a pair's Auto VPN
   * on the **primary's** serial, and the collector reads a row only while its own device is online —
   * so while the site runs on its spare, neither MX has a reading, and an absent line would read as
   * "no VPN here" rather than "not readable now".
   */
  vpnNotRead: boolean;
}

/** Keyed by the state so a sixth one cannot land without a colour. `running_on_spare` is a
 *  warning rather than critical: the site is carrying traffic, and the primary's own liveness alert
 *  already says the rest. */
const PAIR_TONE: Record<MerakiPairState, MerakiPairLine['tone']> = {
  normal: 'ok',
  running_on_spare: 'warning',
  spare_down: 'warning',
  both_down: 'critical',
  unknown: null,
};

/**
 * What the pair line says, or `null` for an MX with no pair. `hasVpn` is whether the card drew a VPN
 * line from this node's own readings — a reading that is there needs no note.
 */
export function merakiPairLine(
  pair: MerakiPair | null | undefined,
  hasVpn: boolean,
): MerakiPairLine | null {
  if (!pair) return null;
  const p = pair.partner;
  return {
    role: pair.role,
    state: pair.state,
    partner: p ? { name: p.name, role: p.role ?? null, nodeId: p.node_id ?? null } : null,
    tone: PAIR_TONE[pair.state],
    vpnNotRead: !hasVpn && pair.state === 'running_on_spare',
  };
}
