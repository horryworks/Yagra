// SPDX-License-Identifier: AGPL-3.0-only
/** Grouping and series assembly for the System Health "Host resources" sections.
 *
 *  The page used to show one instance at a time behind a `<select>`, so comparing a poller against
 *  core — or against its own pool-mates — meant switching and remembering. These functions turn the
 *  flat `/system/hosts` list into **one section for core and one per pool**, and overlay every host
 *  in a section onto the same charts.
 *
 *  Two rules live here because they are the only ones with judgement in them, and both are
 *  invisible to the type system:
 *
 *  1. **Colour is assigned by position within the section**, so a pool of six stays inside the
 *     palette even on a fleet with thirty pollers. Past six the palette repeats; the legend is what
 *     distinguishes them. Nothing is dropped — a host you cannot see is worse than a colour you
 *     have seen before.
 *  2. **A section holding one host keeps the 1m/5m/15m load detail**; two or more collapse to
 *     `load1` so the colour can mean the host instead. There is no reason to discard detail when
 *     there is nothing to compare against.
 *
 *  Lives in a `.ts` for the reason `diskHeadline.ts` gives: Vitest here runs `environment: 'node'`
 *  with `include: ['src/ **\/*.test.ts']`, so anything left inside `SystemHealthPage.tsx` cannot be
 *  reached by a test. `palette` is injected rather than imported, following `flowTrendSeries` — it
 *  keeps this layer free of chart dependencies and lets a test pin the wrap-around with two colours.
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

/** Core, or one pool's pollers. `key` is stable across refreshes so React and the expand/collapse
 *  state can be keyed by it. */
export interface HostSection {
  key: string;
  kind: 'core' | 'pool';
  /** The pool name for a `pool` section, `null` for core. */
  pool: string | null;
  /** Sorted by instance id, which is what makes the colour assignment stable. */
  hosts: HostInfo[];
}

/** Split the host inventory into core and per-pool sections.
 *
 *  Core first, then pools by name; hosts inside a section by instance id. Both orderings exist for
 *  the same reason — the colour a host gets is its index here, so an unstable order would recolour
 *  the charts on every 15s refresh. A poller arriving or leaving still shifts the hosts after it;
 *  that is the honest cost of not persisting a colour per instance forever. */
export function groupHosts(hosts: HostInfo[]): HostSection[] {
  const core = hosts.filter((h) => h.role === 'core');
  const byPool = new Map<string, HostInfo[]>();
  for (const h of hosts) {
    if (h.role === 'core') continue;
    // A poller always carries its pool; `??` is here so a malformed row groups somewhere visible
    // rather than disappearing from the page entirely.
    const pool = h.pool ?? '';
    const list = byPool.get(pool);
    if (list) list.push(h);
    else byPool.set(pool, [h]);
  }
  const byInstance = (a: HostInfo, b: HostInfo) => a.instance.localeCompare(b.instance);
  const sections: HostSection[] = [];
  if (core.length > 0) {
    sections.push({ key: 'core', kind: 'core', pool: null, hosts: [...core].sort(byInstance) });
  }
  for (const pool of [...byPool.keys()].sort((a, b) => a.localeCompare(b))) {
    sections.push({
      key: `pool:${pool}`,
      kind: 'pool',
      pool,
      hosts: (byPool.get(pool) ?? []).sort(byInstance),
    });
  }
  return sections;
}

/** One host's trends, or `null` while its fetch is outstanding or has failed.
 *
 *  A missing range keeps its entry rather than being filtered out: dropping it would shift every
 *  later host's colour, and an empty series in the legend says "no history for this one" where an
 *  absent row says nothing at all. */
export interface HostEntry {
  label: string;
  range: HostMetricRange | null;
}

/** Every filesystem mount any host in the section reports, in first-seen order.
 *
 *  Mounts genuinely differ within a section — core carries `root`, `metrics` and the `database`
 *  size proxy while a poller carries only `root` — so the card list is a union and a host with no
 *  reading for a mount simply draws no line on it. */
export function mountUnion(entries: HostEntry[]): string[] {
  const seen = new Set<string>();
  const mounts: string[] = [];
  for (const e of entries) {
    for (const d of e.range?.disks ?? []) {
      if (seen.has(d.mount)) continue;
      seen.add(d.mount);
      mounts.push(d.mount);
    }
  }
  return mounts;
}

/** Overlay per-host point lists onto one shared timestamp axis.
 *
 *  The axis is the union of every host's timestamps, so hosts whose polls landed on different
 *  seconds still share an X axis; a host with no reading at a given timestamp gets `null`, which
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

/** Memory used-% as points, so it can share an axis with the other hosts.
 *
 *  `pctSeries` returns its own axis and a gapless `number[]`, which is right for a single series
 *  and wrong for an overlay — so its output is folded back into points and the axis work is left to
 *  `overlaySeries`. Its two drop rules (no matching size, non-positive size) are what we want and
 *  are already tested, hence reusing it rather than re-deriving the division here. */
export function memPctPoints(range: HostMetricRange | null): MetricPoint[] {
  if (!range) return [];
  const { timestamps, values } = pctSeries(range.mem_used_bytes, range.mem_total_bytes);
  return timestamps.map((t, i) => ({ t, v: values[i] }));
}

/** One mount's chart across the section.
 *
 *  `known` is per-mount, not per-host: if *any* host in the section reports a capacity for it, the
 *  card is a percentage and a host reporting size 0 contributes gaps. When no host reports one —
 *  the PostgreSQL `database` proxy — the card falls back to a bare-bytes trend, because a
 *  percentage of an unknown capacity would be an invention. */
export interface DiskChart {
  mount: string;
  known: boolean;
  timestamps: number[];
  series: OverlaySeries[];
}

function diskPoints(disk: HostDiskRange | undefined, known: boolean): MetricPoint[] {
  if (!disk) return [];
  if (!known) return disk.used_bytes;
  const { timestamps, values } = pctSeries(disk.used_bytes, disk.size_bytes);
  return timestamps.map((t, i) => ({ t, v: values[i] }));
}

/** Everything the section's cards need, so the `.tsx` holds layout and nothing else. */
export interface SectionCharts {
  cpu: { timestamps: number[]; series: OverlaySeries[] };
  /** `multi` is false only when the section holds one host, in which case `series` is 1m/5m/15m
   *  for that host rather than one line per host. The caller uses it to pick the card title. */
  load: { timestamps: number[]; series: OverlaySeries[]; multi: boolean };
  mem: { timestamps: number[]; series: OverlaySeries[] };
  disks: DiskChart[];
}

export function sectionCharts(entries: HostEntry[], palette: string[]): SectionCharts {
  const cpu = overlaySeries(
    entries.map((e) => ({ label: e.label, points: e.range?.cpu_pct ?? [] })),
    palette,
  );
  const mem = overlaySeries(
    entries.map((e) => ({ label: e.label, points: memPctPoints(e.range) })),
    palette,
  );
  // Rule 2: nothing to compare against ⇒ keep the 1m/5m/15m detail the single-instance card had.
  const multi = entries.length > 1;
  const single = entries[0];
  const load = multi
    ? {
        ...overlaySeries(
          entries.map((e) => ({ label: e.label, points: e.range?.load1 ?? [] })),
          palette,
        ),
        multi,
      }
    : {
        ...overlaySeries(
          [
            { label: '1m', points: single?.range?.load1 ?? [] },
            { label: '5m', points: single?.range?.load5 ?? [] },
            { label: '15m', points: single?.range?.load15 ?? [] },
          ],
          palette,
        ),
        multi,
      };
  const disks = mountUnion(entries).map((mount) => {
    const known = entries.some((e) =>
      (e.range?.disks ?? []).some((d) => d.mount === mount && d.size_bytes.some((p) => p.v > 0)),
    );
    return {
      mount,
      known,
      ...overlaySeries(
        entries.map((e) => ({
          label: e.label,
          points: diskPoints(
            (e.range?.disks ?? []).find((d) => d.mount === mount),
            known,
          ),
        })),
        palette,
      ),
    };
  });
  return { cpu, load, mem, disks };
}

/** The host in the section currently reading highest for one measure, or `null` when none reports.
 *
 *  Multi-host cards cannot show a single headline number honestly, so they show the worst one and
 *  name it — which is the question a pool section is opened to answer ("is anything in here hot?").
 *  Single-host sections keep their own reading and do not use this. */
export function peakOf(
  hosts: HostInfo[],
  pick: (h: HostInfo) => number | null | undefined,
): { instance: string; value: number } | null {
  let best: { instance: string; value: number } | null = null;
  for (const h of hosts) {
    const v = pick(h);
    if (v == null || !Number.isFinite(v)) continue;
    if (!best || v > best.value) best = { instance: h.instance, value: v };
  }
  return best;
}

/** A host's current used-% for one mount, for `peakOf` and the collapsed summary row. `null` when
 *  the host does not report the mount, or reports it without a capacity. */
export function mountPct(h: HostInfo, mount: string): number | null {
  const d = h.disks.find((x) => x.mount === mount);
  if (!d || d.size_bytes <= 0) return null;
  return (d.used_bytes / d.size_bytes) * 100;
}

/** A multi-host card's headline: the worst current reading in the section, and whose it is.
 *
 *  One number cannot describe several hosts honestly, and "is anything in this pool hot" is the
 *  question a pool section is opened to answer — so the headline names the peak rather than
 *  averaging it away or going blank. Single-host sections keep their own reading instead. */
export function peakHeadline(
  hosts: HostInfo[],
  pick: (h: HostInfo) => number | null | undefined,
  format: (v: number) => string,
): string {
  const peak = peakOf(hosts, pick);
  return peak ? `${format(peak.value)} · ${peak.instance}` : '—';
}
