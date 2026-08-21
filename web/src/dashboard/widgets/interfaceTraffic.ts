// SPDX-License-Identifier: AGPL-3.0-only
// What the "Interface traffic" widget should do, given its persisted selection and the interface
// rosters of the nodes it names (ADR-069).
//
// This widget answers the question ADR-046 Inc.2 deliberately refused: `metricChart.ts` excludes
// interface-dimensioned metrics because "collapsing eight ports into one line answers a different
// question, worse". The right answer to that different question is not to collapse but to let the
// operator name the links — across nodes — and draw each one.
//
// A `.ts` on purpose: Vitest runs `environment: 'node'` with `include: ['src/**/*.test.ts']`, so
// judgement left in the `.tsx` is judgement nothing tests.

import { throughputPair } from '../../components/NodeDetail/interfaceMetrics';
import type { ChartSeries } from '../../components/MetricChart/MetricChart';
import type { RateUnit } from '../../prefs';
import type { InterfaceRow, InterfaceSeries } from '../../types/api';
import type { WidgetSettings } from '../types';

/**
 * How many links one widget may plot.
 *
 * This is `MetricChart.PALETTE.length`, and the coupling is the point: a seventh link would wrap to
 * the first colour, and a legend naming a colour the line does not use is a failure that *looks
 * like a working chart* — the trap ADR-046 Inc.5 pinned a test against. Capping the feature at the
 * palette makes that failure unreachable rather than merely unlikely.
 */
export const MAX_LINKS = 6;

/** The time windows this widget offers, as a subset of the app-wide `RangeControl.RANGES`.
 *
 *  Labels are untranslated, exactly as they are in the shared list — `1h` reads the same in both
 *  locales, and a runtime-built `t()` key is the shape that renders a raw key when someone forgets
 *  a string (`extensibility.md` §4).
 *
 *  `3d` is left out so the card header keeps room for three controls at its narrowest allowed span.
 *  The subset relation is pinned by a test rather than by an import: the shared list lives in a
 *  `.tsx`, and this module is on the widget's runtime path (the test is not). A window added to the
 *  shared list therefore forces the question here instead of silently diverging — the
 *  `monitorKinds.ts` shape, where the registry is a deliberate subset of a larger set. */
export const TRAFFIC_RANGES: readonly { label: string; secs: number }[] = [
  { label: '1h', secs: 3600 },
  { label: '6h', secs: 6 * 3600 },
  { label: '24h', secs: 24 * 3600 },
  { label: '7d', secs: 7 * 86400 },
];

/** Default window for a freshly added widget: the shortest one, which is also the only one whose
 *  pps series is guaranteed to be populated on a deployment that upgraded recently (ADR-060 started
 *  collecting the packet counters, so a 7d pps window is mostly empty by construction). */
export const DEFAULT_RANGE_SECS = 3600;

/**
 * How often to re-fetch, per window.
 *
 * Not the dashboard's flat 15s: the server picks `step = max(60, span/120)`, so a 7d window
 * redraws the *same* 120 points however often it is asked. Each poll costs one HTTP call and ten
 * TSDB range queries **per link**, so paying 15s for a week-long window is pure waste (ADR-069
 * decision 5).
 */
export function refreshMsFor(rangeSecs: number): number {
  if (rangeSecs <= 3600) return 15_000;
  if (rangeSecs <= 6 * 3600) return 60_000;
  if (rangeSecs <= 24 * 3600) return 300_000;
  return 900_000;
}

/** One picked interface. `nodeName`/`ifName` are snapshots taken when it was picked, used as a
 *  provisional label until the node's roster arrives — never as the identity, which is the
 *  `(nodeId, ifindex)` pair alone. */
export interface LinkRef {
  nodeId: string;
  nodeName: string | null;
  ifindex: number;
  ifName: string | null;
}

/** The widget's persisted selection. */
export interface TrafficSelection {
  links: LinkRef[];
  unit: RateUnit;
  rangeSecs: number;
}

const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);

/**
 * Read a selection out of the opaque settings bag, dropping anything malformed.
 *
 * The bag is `Record<string, unknown>`: user-editable JSON that has round-tripped through
 * localStorage and the server. A string where an ifindex belongs must degrade to "that link was
 * never picked", not to a request for `/interfaces/NaN/series`. Duplicates are collapsed and the
 * list is truncated to {@link MAX_LINKS}, so a hand-edited document cannot exceed the palette.
 */
export function readTrafficSettings(settings: WidgetSettings | undefined): TrafficSelection {
  const raw = Array.isArray(settings?.links) ? settings.links : [];
  const links: LinkRef[] = [];
  const seen = new Set<string>();
  for (const item of raw) {
    if (typeof item !== 'object' || item === null) continue;
    const rec = item as Record<string, unknown>;
    const nodeId = str(rec.nodeId);
    const ifindex = rec.ifindex;
    // `Number.isInteger` rejects '7', 7.5, NaN and Infinity in one go. ifIndex 0 is not a valid
    // SNMP interface index, so a falsy-but-integer 0 is dropped too.
    if (!nodeId || typeof ifindex !== 'number' || !Number.isInteger(ifindex) || ifindex <= 0) continue;
    const key = linkId({ nodeId, ifindex });
    if (seen.has(key)) continue;
    seen.add(key);
    links.push({ nodeId, nodeName: str(rec.nodeName), ifindex, ifName: str(rec.ifName) });
    if (links.length >= MAX_LINKS) break;
  }
  const unit: RateUnit = settings?.unit === 'pps' ? 'pps' : 'bps';
  const asked = settings?.rangeSecs;
  const rangeSecs =
    typeof asked === 'number' && TRAFFIC_RANGES.some((r) => r.secs === asked)
      ? asked
      : DEFAULT_RANGE_SECS;
  return { links, unit, rangeSecs };
}

/** The identity of a link, and the only spelling of it.
 *
 *  Built here rather than at the call sites because the same string is a React key, a `usePolled`
 *  dependency and a de-duplication key — and a pair of functions that build and split the same
 *  composite with different separators is a bug this repo has already shipped once (ADR-046's
 *  `useEffect` key, which joined on NUL and split on whitespace, so it only worked for one item). */
export function linkId(l: Pick<LinkRef, 'nodeId' | 'ifindex'>): string {
  return `${l.nodeId}:${l.ifindex}`;
}

/** A stable dependency key for the whole selection. Passing the array itself to `usePolled` would
 *  re-arm the fetch on every render, because a fresh array is parsed out of the settings bag each
 *  time. */
export function linksKey(links: readonly LinkRef[]): string {
  return links.map(linkId).join(',');
}

/** The distinct nodes a selection touches, in first-picked order — one roster fetch each. */
export function selectedNodeIds(links: readonly LinkRef[]): string[] {
  const out: string[] = [];
  for (const l of links) if (!out.includes(l.nodeId)) out.push(l.nodeId);
  return out;
}

/** The rows of `nodeId` that are not already plotted, so the picker cannot add a duplicate. */
export function availableInterfaces(
  rows: readonly InterfaceRow[],
  links: readonly LinkRef[],
  nodeId: string,
): InterfaceRow[] {
  const taken = new Set(links.filter((l) => l.nodeId === nodeId).map((l) => l.ifindex));
  return rows.filter((r) => !taken.has(r.ifindex));
}

/** Human label for an interface row. Both name columns are nullable on every vendor, so the
 *  ifindex is the last-resort label — the same fallback the Interfaces tab and the heatmap use. */
export function interfaceLabel(ifindex: number, ifName?: string | null, ifAlias?: string | null): string {
  return ifName || ifAlias || `if${ifindex}`;
}

/** `node · interface`, the label a chart series carries. */
export function linkLabel(nodeName: string | null, ifLabel: string): string {
  return nodeName ? `${nodeName} · ${ifLabel}` : ifLabel;
}

/** A link that resolved against its node's current roster, with a fresh label. */
export interface ResolvedLink extends LinkRef {
  /** `node · interface`, rebuilt from the roster rather than from the persisted snapshot. */
  label: string;
}

/**
 * One node's interface roster, as the widget knows it.
 *
 * Four states, and collapsing any pair of them tells the operator something untrue:
 *  - `undefined` / `null` — still loading. Not "gone": conflating it with `[]` would flash
 *    "no longer reported" every time a link is added.
 *  - `InterfaceRow[]` — the node's current interfaces. An empty array is a real answer.
 *  - `'failed'` — the roster request itself failed. **The links stay drawn with their saved
 *    labels.** A transient 500 must not blank a working chart, and it certainly must not be
 *    reported as "this interface no longer exists" — we did not learn that.
 */
export type RosterState = InterfaceRow[] | null | 'failed';

/** What the widget body should render this pass. */
export type TrafficPlan =
  /** Nothing picked yet. */
  | { kind: 'empty' }
  /** Links are picked but at least one node's roster has not arrived, so no label is trustworthy. */
  | { kind: 'loading' }
  /** Draw these. `unavailable` names the picked links whose ifindex the node no longer reports. */
  | { kind: 'chart'; links: ResolvedLink[]; unavailable: string[] };

/**
 * Decide what to render.
 *
 * `roster[nodeId] === undefined` (or `null`) means the list is still loading — deliberately
 * distinct from `[]`, which means the node genuinely reports no interfaces and a persisted link is
 * therefore stale. Conflating the two would flash "no longer reported" at the operator every time
 * they add a link.
 *
 * ⚠️ ifIndex is not stable across reboots on many devices, so a saved selection *will* point at a
 * missing row eventually. Every link that still resolves is drawn; the ones that do not are named,
 * because a link that silently disappears from a six-line chart is indistinguishable from a link
 * that has gone quiet.
 */
export function interfaceTrafficPlan(
  sel: TrafficSelection,
  roster: Readonly<Record<string, RosterState>>,
): TrafficPlan {
  if (sel.links.length === 0) return { kind: 'empty' };
  const nodes = selectedNodeIds(sel.links);
  if (nodes.some((id) => roster[id] == null)) return { kind: 'loading' };

  const links: ResolvedLink[] = [];
  const unavailable: string[] = [];
  for (const l of sel.links) {
    const rows = roster[l.nodeId];
    const saved = linkLabel(l.nodeName, interfaceLabel(l.ifindex, l.ifName));
    if (rows === 'failed') {
      links.push({ ...l, label: saved });
      continue;
    }
    const row = (rows ?? []).find((r) => r.ifindex === l.ifindex);
    if (!row) {
      unavailable.push(saved);
      continue;
    }
    links.push({
      ...l,
      label: linkLabel(l.nodeName, interfaceLabel(row.ifindex, row.if_name, row.if_alias)),
    });
  }
  return { kind: 'chart', links, unavailable };
}

/** One link's fetched series, paired with the resolved link it belongs to. A fetch that failed
 *  carries `series: null` — one unreachable node must not blank the whole chart. */
export interface LinkSeries {
  link: ResolvedLink;
  series: InterfaceSeries | null;
}

/** Labels for the two directions of one link, supplied by the caller so they are translated. */
export interface DirectionLabels {
  in: string;
  out: string;
}

/**
 * Build the chart's shared x-axis and its series.
 *
 * Three things here are load-bearing:
 *
 *  1. **Out is negated.** Receive occupies the positive half and transmit the negative half, so one
 *     link needs one colour and six links fit the palette (ADR-069 decisions 1 and 2). `null` stays
 *     `null` — a gap is a hole, not a valley, and turning it into `0` draws traffic that never
 *     happened.
 *  2. **The unit picks the arrays through `throughputPair`,** not through a local branch. All four
 *     candidate arrays have the same type, so a swapped pair compiles and renders a pps axis drawn
 *     from bps values (which is why ADR-060 put that choice in one tested function).
 *  3. **Each link's response carries its own `timestamps`.** The caller asks every link for the
 *     same `from`/`to`/`step`, so they normally agree — but "normally" is not a guarantee, and a
 *     mismatch would shift one link's history against the others while still drawing a plausible
 *     chart. Values are placed by timestamp, not by array position.
 *
 * The x-axis is the first link that returned data; links whose fetch failed contribute no series.
 */
export function buildTrafficSeries(
  entries: readonly LinkSeries[],
  unit: RateUnit,
  palette: readonly string[],
  labels: DirectionLabels,
): { timestamps: number[]; series: ChartSeries[] } {
  const axis = entries.find((e) => e.series != null && e.series.timestamps.length > 0)?.series;
  if (!axis) return { timestamps: [], series: [] };
  const timestamps = axis.timestamps;
  const series: ChartSeries[] = [];

  // Colour by position in the *selection*, not among the links that answered: a link whose fetch
  // failed this tick must not shift the remaining links onto each other's colours.
  entries.forEach((entry, i) => {
    const s = entry.series;
    if (s == null || s.timestamps.length === 0) return;
    const color = palette[i % palette.length];
    const [inRaw, outRaw] = throughputPair(s, unit);
    // One placement path, always by timestamp — a fast "same length, same order" branch would be a
    // second implementation of the same rule, and the slow one is a handful of map lookups.
    const align = (values: (number | null)[], sign: 1 | -1): (number | null)[] => {
      const byTs = new Map<number, number | null>();
      s.timestamps.forEach((ts, j) => byTs.set(ts, values[j] ?? null));
      return timestamps.map((ts) => {
        const v = byTs.get(ts);
        return v == null ? null : sign * v;
      });
    };
    series.push({ label: `${entry.link.label} ${labels.in}`, values: align(inRaw, 1), color });
    series.push({ label: `${entry.link.label} ${labels.out}`, values: align(outRaw, -1), color });
  });

  return { timestamps, series };
}
