// SPDX-License-Identifier: AGPL-3.0-only
/** Grouping and series assembly for the System Health "Host resources" sections.
 *
 *  **One section is one host.** The page used to show one instance at a time behind a `<select>`,
 *  then briefly one section per *pool* with every poller in it overlaid on shared charts. Both
 *  answered "is anything in here hot" and neither answered "what is *this* poller doing" — with two
 *  pollers reading 0.1%/0.1%, 14%/13% and 8.6%/7.6% the lines sat on top of each other and the
 *  headline named only the worst one, so the page read as a pool-level gauge (ADR-118).
 *
 *  The one rule with judgement in it, and it is invisible to the type system:
 *
 *  > **Colour means a series' role inside its card, never the host.** A section holds one host, so
 *  > nothing competes with the host for a colour. On the load card the role is the window — `1m`,
 *  > `5m`, `15m` take the first three palette entries. On the two network cards it is the path —
 *  > Core ⇄ poller takes the first, everything else the second — and **direction is not a colour at
 *  > all**: received is drawn above the axis and sent below it (ADR-137, the shape ADR-069 decision 2
 *  > gave the interface-traffic widget). Four colours would have put the network cards at odds with
 *  > `SERIES_IN` / `SERIES_OUT`, where the first two palette entries mean in and out.
 *
 *  ⚠️ `overlaySeries` survived the split and is not vestigial: the load card still puts three
 *  series on one axis, the network cards four, and those point lists need not share timestamps.
 *
 *  ⚠️ **A network point is the bytes moved during one step, not a rate** (`HostNetworkRange`). The
 *  speed card divides by `step_secs`; the cumulative card adds the points up. "Other" is the
 *  interface total minus the bus share, taken here rather than on the server so that a step missing
 *  either side stays a gap instead of reading as zero traffic.
 *
 *  Lives in a `.ts` for the reason `diskHeadline.ts` gives: Vitest here runs `environment: 'node'`
 *  with an `src/**` + `.test.ts` include, so anything left inside `SystemHealthPage.tsx` cannot be
 *  reached by a test. `palette` and the network labels are injected rather than imported, following
 *  `flowTrendSeries` — it keeps this layer free of chart and i18n dependencies and lets a test pin
 *  the assignment with two colours.
 */

import { alignTo, pctSeries } from '../lib/seriesMath';
import type { HostDiskRange, HostInfo, HostMetricRange, MetricPoint } from '../types/api';

/** One chart series, structurally the `ChartSeries` MetricChart consumes (declared here so this
 *  module has no chart import). A `null` value is a gap — never a zero. */
export interface OverlaySeries {
  label: string;
  values: (number | null)[];
  color: string;
}

/** One host — core, or one poller. `key` is stable across refreshes so React and the
 *  expand/collapse state can be keyed by it. */
export interface HostSection {
  key: string;
  kind: 'core' | 'pool';
  /** The pool name for a poller, `null` for core. Drives the heading and nothing else. */
  pool: string | null;
  host: HostInfo;
  /** True only when more than one core reports — an HA pair. The heading names the instance in
   *  that case alone, because `Core / Web` is the name of a role and `Core / Web · core` stutters. */
  coreAmbiguous: boolean;
}

/** One section per host: core(s) first, then pollers by pool name and by instance within a pool.
 *
 *  The ordering is what keeps the page from reshuffling under the 15s inventory refresh. It also
 *  costs less than the per-pool overlay it replaced: a poller arriving or leaving now inserts or
 *  removes a section, where before it shifted every later host's colour (ADR-118). */
export function groupHosts(hosts: HostInfo[]): HostSection[] {
  const byInstance = (a: HostInfo, b: HostInfo) => a.instance.localeCompare(b.instance);
  const cores = hosts.filter((h) => h.role === 'core').sort(byInstance);
  const pollers = hosts
    .filter((h) => h.role !== 'core')
    // A poller always carries its pool; `??` is here so a malformed row sorts somewhere visible
    // rather than disappearing from the page entirely.
    .sort((a, b) => (a.pool ?? '').localeCompare(b.pool ?? '') || byInstance(a, b));
  const coreAmbiguous = cores.length > 1;
  return [
    ...cores.map((host) => ({
      key: `host:${host.instance}`,
      kind: 'core' as const,
      pool: null,
      host,
      coreAmbiguous,
    })),
    ...pollers.map((host) => ({
      key: `host:${host.instance}`,
      kind: 'pool' as const,
      pool: host.pool ?? '',
      host,
      coreAmbiguous: false,
    })),
  ];
}

/** Overlay per-window point lists onto one shared timestamp axis.
 *
 *  The axis is the union of every series' timestamps, so windows whose samples landed on different
 *  seconds still share an X axis; a series with no reading at a given timestamp gets `null`, which
 *  uPlot draws as a gap rather than a cliff to zero (`alignTo`'s contract). */
export function overlaySeries(
  points: { label: string; points: MetricPoint[] }[],
  palette: string[],
): { timestamps: number[]; series: OverlaySeries[] } {
  const tsSet = new Set<number>();
  for (const p of points) for (const q of p.points) tsSet.add(q.t);
  const timestamps = [...tsSet].sort((a, b) => a - b);
  const series = points.map((p, i) => ({
    label: p.label,
    values: alignTo(timestamps, p.points),
    color: palette[i % palette.length],
  }));
  return { timestamps, series };
}

/** Memory used-% as points, so it goes through the same axis assembly as everything else.
 *
 *  `pctSeries` returns its own axis and a gapless `number[]`; folding its output back into points
 *  and leaving the axis work to `overlaySeries` keeps one path through this module. Its two drop
 *  rules (no matching size, non-positive size) are what we want and are already tested, hence
 *  reusing it rather than re-deriving the division here. */
export function memPctPoints(range: HostMetricRange | null): MetricPoint[] {
  if (!range) return [];
  const { timestamps, values } = pctSeries(range.mem_used_bytes, range.mem_total_bytes);
  return timestamps.map((t, i) => ({ t, v: values[i] }));
}

/** One mount's chart.
 *
 *  `known` says whether this host reports a capacity for the mount: with one, the card is a
 *  percentage; without one — the PostgreSQL `database` size proxy — it falls back to a bare-bytes
 *  trend, because a percentage of an unknown capacity would be an invention. */
export interface DiskChart {
  mount: string;
  known: boolean;
  timestamps: number[];
  series: OverlaySeries[];
}

function diskPoints(disk: HostDiskRange, known: boolean): MetricPoint[] {
  if (!known) return disk.used_bytes;
  const { timestamps, values } = pctSeries(disk.used_bytes, disk.size_bytes);
  return timestamps.map((t, i) => ({ t, v: values[i] }));
}

/** "Other" traffic per step: the interface total minus the bus share, at the steps that have both.
 *
 *  A step with only one side is dropped — a gap — because the difference is not known there; an
 *  interface reading missing beside a bus reading would otherwise draw as zero traffic, which is
 *  exactly the reading nobody took. Floored at 0: the bus counts payloads and the interface counts
 *  the wire, sampled a moment apart, so a quiet step can come out a few bytes below zero. */
export function otherPoints(total: MetricPoint[], bus: MetricPoint[]): MetricPoint[] {
  const busByT = new Map(bus.map((p) => [p.t, p.v]));
  const out: MetricPoint[] = [];
  for (const p of total) {
    const b = busByT.get(p.t);
    if (b == null) continue;
    out.push({ t: p.t, v: Math.max(0, p.v - b) });
  }
  return out;
}

/** Bytes per step → bits per second. Nothing at all for a step that is not positive: dividing by it
 *  yields Infinity or NaN, which uPlot draws. */
export function bpsPoints(points: MetricPoint[], stepSecs: number): MetricPoint[] {
  if (!(stepSecs > 0)) return [];
  return points.map((p) => ({ t: p.t, v: (p.v / stepSecs) * 8 }));
}

/** Running total of per-step bytes, oldest first. A step the store has no point for adds nothing,
 *  and on the shared axis it stays a gap rather than a flat line drawn through it. */
export function cumulativePoints(points: MetricPoint[]): MetricPoint[] {
  let sum = 0;
  return points.map((p) => {
    sum += p.v;
    return { t: p.t, v: sum };
  });
}

/** Sent traffic is drawn below the axis. */
export function below(points: MetricPoint[]): MetricPoint[] {
  return points.map((p) => ({ t: p.t, v: -p.v }));
}

/** What the four network series are called in the legend. Injected like `palette`. */
export interface NetLabels {
  busIn: string;
  busOut: string;
  otherIn: string;
  otherOut: string;
}

const NET_LABEL_IDS: NetLabels = {
  busIn: 'bus-in',
  busOut: 'bus-out',
  otherIn: 'other-in',
  otherOut: 'other-out',
};

/** A network card's headline: the interface as a whole, received and sent. `null` where there is
 *  nothing to say, which the page renders as `—` rather than as zero. */
export interface NetReading {
  rx: number | null;
  tx: number | null;
}

type NetworkRange = HostMetricRange['network'];

/** The four network series on one axis. Colour is the path (`palette[0]` the bus, `palette[1]`
 *  everything else); `perStep` turns a per-step byte list into what the card draws. */
function netSeries(
  n: NetworkRange | undefined,
  perStep: (points: MetricPoint[]) => MetricPoint[],
  labels: NetLabels,
  palette: string[],
): { timestamps: number[]; series: OverlaySeries[] } {
  const nicRx = n?.nic_rx_bytes ?? [];
  const nicTx = n?.nic_tx_bytes ?? [];
  const busRx = n?.bus_rx_bytes ?? [];
  const busTx = n?.bus_tx_bytes ?? [];
  const parts = [
    { label: labels.busIn, points: perStep(busRx), path: 0 },
    { label: labels.busOut, points: below(perStep(busTx)), path: 0 },
    { label: labels.otherIn, points: perStep(otherPoints(nicRx, busRx)), path: 1 },
    { label: labels.otherOut, points: below(perStep(otherPoints(nicTx, busTx))), path: 1 },
  ];
  const { timestamps, series } = overlaySeries(parts, palette);
  return {
    timestamps,
    series: series.map((s, i) => ({ ...s, color: palette[parts[i].path % palette.length] })),
  };
}

const lastValue = (points: MetricPoint[]): number | null =>
  points.length > 0 ? points[points.length - 1].v : null;

const totalValue = (points: MetricPoint[]): number | null =>
  points.length > 0 ? points.reduce((sum, p) => sum + p.v, 0) : null;

/** Everything one host's cards need, so the `.tsx` holds layout and nothing else. */
export interface HostCharts {
  cpu: { timestamps: number[]; series: OverlaySeries[] };
  /** Always 1m/5m/15m. A section is one host, so the colour is free to mean the window. */
  load: { timestamps: number[]; series: OverlaySeries[] };
  mem: { timestamps: number[]; series: OverlaySeries[] };
  /** In the order the host reports its mounts, which is the order the collector writes them. */
  disks: DiskChart[];
  /** Bits per second, per path; received above the axis and sent below. */
  net: { timestamps: number[]; series: OverlaySeries[] };
  /** Bytes since the start of the window, per path; received above the axis and sent below. */
  netTotal: { timestamps: number[]; series: OverlaySeries[] };
  /** The interface's most recent rate, in bits per second — the speed card's headline. */
  netRate: NetReading;
  /** The interface's total over the window, in bytes — the cumulative card's headline. */
  netSum: NetReading;
}

/** `range` is `null` while the host's fetch is outstanding or has failed — the cards then draw
 *  empty rather than vanishing, so a section that is loading stays distinguishable from a host
 *  that reports nothing. */
export function hostCharts(
  range: HostMetricRange | null,
  palette: string[],
  netLabels: NetLabels = NET_LABEL_IDS,
): HostCharts {
  const one = (label: string, points: MetricPoint[]) => overlaySeries([{ label, points }], palette);
  const n = range?.network;
  const step = n?.step_secs ?? 0;
  const toBps = (points: MetricPoint[]) => bpsPoints(points, step);
  return {
    cpu: one('cpu', range?.cpu_pct ?? []),
    load: overlaySeries(
      [
        { label: '1m', points: range?.load1 ?? [] },
        { label: '5m', points: range?.load5 ?? [] },
        { label: '15m', points: range?.load15 ?? [] },
      ],
      palette,
    ),
    mem: one('mem', memPctPoints(range)),
    disks: (range?.disks ?? []).map((d) => {
      const known = d.size_bytes.some((p) => p.v > 0);
      return { mount: d.mount, known, ...one(d.mount, diskPoints(d, known)) };
    }),
    net: netSeries(n, toBps, netLabels, palette),
    netTotal: netSeries(n, cumulativePoints, netLabels, palette),
    netRate: {
      rx: lastValue(toBps(n?.nic_rx_bytes ?? [])),
      tx: lastValue(toBps(n?.nic_tx_bytes ?? [])),
    },
    netSum: {
      rx: totalValue(n?.nic_rx_bytes ?? []),
      tx: totalValue(n?.nic_tx_bytes ?? []),
    },
  };
}
