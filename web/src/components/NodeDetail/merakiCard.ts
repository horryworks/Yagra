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
import { POSITIVE_HALF } from '../../dashboard/widgets/interfaceTraffic';
import { alignTo } from '../../lib/seriesMath';
import type { MerakiHaRole, MerakiPair, MerakiPairState, MetricPoint } from '../../types/api';
import type { ChartSeries } from '../MetricChart/MetricChart';

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
 * than something wrong. A Rust test reads this switch and pins each `case` to the value the
 * collector writes (`api/meraki.rs::the_cards_uplink_words_read_the_values_the_collector_writes`),
 * so keep each case a number and each return a plain quoted word.
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
   * The VPN line says "not read" instead of a reading. Meraki reports a pair's Auto VPN on the
   * **primary's** serial, and the collector reads a row only while its own device is online — so
   * while the site runs on its spare, nothing current exists for either MX.
   *
   * ⚠️ **A reading can still be there, and it is stale.** The latest-value read looks back 30
   * minutes (`last_over_time[1800s]`), so the primary's card kept "2 of 2 hubs reachable" from
   * before the failover beside "Running on spare" — measured on a lab deployment. Such a reading is
   * hidden, not shown.
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

/** What the pair line says, or `null` for an MX with no pair. */
export function merakiPairLine(pair: MerakiPair | null | undefined): MerakiPairLine | null {
  if (!pair) return null;
  const p = pair.partner;
  return {
    role: pair.role,
    state: pair.state,
    partner: p ? { name: p.name, role: p.role ?? null, nodeId: p.node_id ?? null } : null,
    tone: PAIR_TONE[pair.state],
    vpnNotRead: pair.state === 'running_on_spare',
  };
}

/** One uplink's stored history, as the card read it back (ADR-164 決定 27). */
export interface MerakiUplinkHistory {
  row: number;
  name: string;
  sent: readonly MetricPoint[];
  recv: readonly MetricPoint[];
}

/**
 * The WAN traffic chart: one colour per uplink, sending above zero and receiving below it.
 *
 * Which direction is on top is not decided here — it is `POSITIVE_HALF`, the answer every other
 * traffic chart in the product reads, so this one cannot draw the halves the other way round. The
 * card passes the gutter words through `mirrorAxisLabels`, which reads the same constant.
 *
 * - **Placed by timestamp**, not by array position: the sent and received series are separate
 *   reads, and a point missing from one must not shift the other.
 * - **A gap stays a gap** (`null`), never a `0` — a zero is traffic that was measured.
 * - **Colour by the uplink's place in the list**, not among those that answered: an uplink whose
 *   read failed this tick must not move the others onto its colour.
 */
export function merakiTrafficSeries(
  uplinks: readonly MerakiUplinkHistory[],
  palette: readonly string[],
  labels: { sent: string; recv: string },
): { timestamps: number[]; series: ChartSeries[] } {
  const at = new Set<number>();
  for (const u of uplinks) for (const p of [...u.sent, ...u.recv]) at.add(p.t);
  const timestamps = [...at].sort((a, b) => a - b);
  const series: ChartSeries[] = [];
  if (timestamps.length === 0) return { timestamps, series };
  const signed = (points: readonly MetricPoint[], sign: 1 | -1) =>
    alignTo(timestamps, [...points]).map((v) => (v == null ? null : sign * v));
  const sentSign: 1 | -1 = POSITIVE_HALF === 'out' ? 1 : -1;
  uplinks.forEach((u, i) => {
    const color = palette[i % palette.length];
    // The positive half first, so the legend reads in the order the chart draws it: top-down.
    const halves: [string, readonly MetricPoint[], 1 | -1][] =
      sentSign === 1
        ? [
            [labels.sent, u.sent, 1],
            [labels.recv, u.recv, -1],
          ]
        : [
            [labels.recv, u.recv, 1],
            [labels.sent, u.sent, -1],
          ];
    for (const [label, points, sign] of halves) {
      if (points.length === 0) continue;
      series.push({ label: `${u.name} ${label}`, values: signed(points, sign), color });
    }
  });
  return { timestamps, series };
}

/** One radio of a Meraki access point on the card (ADR-168): its band, and the last five minutes'
 *  channel utilization with the part of it that was not Wi-Fi. */
export interface MerakiRadioLine {
  row: number;
  /** What the radio is called — its `interfaces` row's name ("2.4 GHz"), or its row key when the
   *  row has not been written yet. */
  band: string;
  utilPct: number | null;
  nonWifiPct: number | null;
}

/**
 * One line per radio that has either reading, in slot order (2.4 GHz, 5 GHz, 6 GHz, then a band's
 * second radio).
 *
 * The band is the radio's own `interfaces` row name, which the collect writes with the reading —
 * never worked out here from the slot number, which would be a second copy of the numbering in
 * `yagra_common::WlanBand::slot_base`. The two readings are joined by row, as the uplinks are, so
 * a radio missing one does not shift the other onto its neighbour.
 */
export function merakiRadioLines(
  util: readonly RowReading[],
  nonWifi: readonly RowReading[],
  names: ReadonlyMap<number, string>,
): MerakiRadioLine[] {
  const byRow = new Map<number, MerakiRadioLine>();
  const line = (row: number): MerakiRadioLine => {
    let l = byRow.get(row);
    if (!l) {
      l = { row, band: names.get(row) ?? `#${row}`, utilPct: null, nonWifiPct: null };
      byRow.set(row, l);
    }
    return l;
  };
  for (const r of util) line(r.row).utilPct = r.value;
  for (const r of nonWifi) line(r.row).nonWifiPct = r.value;
  return [...byRow.values()].sort((a, b) => a.row - b.row);
}

/**
 * Whether an access point's SSID count and radio readings may be drawn (ADR-168 決定 4).
 *
 * 🚨 **Not while the Dashboard reports it offline.** Those two are the readings the collect stops
 * writing for a stopped access point — its radios are not measured, and the Dashboard keeps
 * answering its last SSID configuration as broadcasting — while the latest-value read looks back
 * thirty minutes (`store.rs::latest_query`). Drawn anyway, the card says "Offline" and
 * "Channel utilization 11%" side by side, which is the defect ADR-164 増分 13d fixed for a warm
 * spare's VPN line and this is the same shape. Its client count is not affected: the Dashboard
 * answers 0 for a stopped access point, and that 0 is stored like any reading.
 *
 * `null` — no availability reading at all — shows them: "we do not know" is not "it is down".
 */
export function merakiApRadioReadingsShown(up: number | null): boolean {
  return up !== 0;
}

/** One line of a history chart before it is aligned: what it is called, its points, its colour. */
interface HistoryLine {
  label: string;
  points: readonly MetricPoint[];
  color: string;
}

/**
 * Lines read back separately, on one time axis. Placed by timestamp — a point missing from one
 * read must not shift another line — and a gap stays `null`, never `0`. A line with no stored
 * point is left out rather than drawn as an empty legend entry.
 */
function alignedLines(lines: readonly HistoryLine[]): { timestamps: number[]; series: ChartSeries[] } {
  const kept = lines.filter((l) => l.points.length > 0);
  const at = new Set<number>();
  for (const l of kept) for (const p of l.points) at.add(p.t);
  const timestamps = [...at].sort((a, b) => a - b);
  const series = kept.map((l) => ({
    label: l.label,
    values: alignTo(timestamps, [...l.points]),
    color: l.color,
  }));
  return { timestamps, series };
}

/** One radio's stored history, as the card read it back (ADR-168). */
export interface MerakiRadioHistory {
  row: number;
  band: string;
  util: readonly MetricPoint[];
  nonWifi: readonly MetricPoint[];
}

/**
 * The two history charts on a Meraki access point's card (ADR-168): how many clients and SSIDs,
 * and each radio's channel utilization with the part of it that was not Wi-Fi.
 *
 * - **Counts and percentages never share an axis** — 30 clients and 30% are different things, and
 *   one scale would make whichever is smaller unreadable.
 * - **Colour by the line's place in its chart**, fixed by the order the radios were read in (slot
 *   order), so a radio that stored nothing this range does not move the others onto its colour.
 * - The history is drawn **even while the access point is offline**: every point carries its own
 *   time, unlike the tiles' latest value (`merakiApRadioReadingsShown`), and "it stopped at 14:05"
 *   is exactly what the chart is for.
 */
export function merakiApHistorySeries(
  clients: readonly MetricPoint[],
  ssids: readonly MetricPoint[],
  radios: readonly MerakiRadioHistory[],
  palette: readonly string[],
  labels: { clients: string; ssids: string; nonWifi: (band: string) => string },
): {
  counts: { timestamps: number[]; series: ChartSeries[] };
  util: { timestamps: number[]; series: ChartSeries[] };
} {
  const color = (i: number) => palette[i % palette.length];
  const counts = alignedLines([
    { label: labels.clients, points: clients, color: color(0) },
    { label: labels.ssids, points: ssids, color: color(1) },
  ]);
  const util = alignedLines(
    radios.flatMap((r, i) => [
      { label: r.band, points: r.util, color: color(2 * i) },
      { label: labels.nonWifi(r.band), points: r.nonWifi, color: color(2 * i + 1) },
    ]),
  );
  return { counts, util };
}

/**
 * An axis tick on a chart of counts: its number when it is a whole one, and nothing otherwise.
 *
 * uPlot puts ticks at fractions when the range is small, and a whole-number formatter then prints
 * them rounded — "4, 4, 3, 3, 2" down the axis of a chart whose values were 2 and 4, seen on the
 * lab deployment. A client or an SSID has no half, so the fractional ticks keep their grid line and
 * say nothing.
 */
export function countAxisLabel(v: number, format: (n: number) => string): string {
  return Number.isInteger(v) ? format(v) : '';
}
