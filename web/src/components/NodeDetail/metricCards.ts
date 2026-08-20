// SPDX-License-Identifier: AGPL-3.0-only
// The Device-health gauges on the node Overview, as data.
//
// Each card was previously a hardcoded `*_METRICS` array, its own `useState` slot, its own copy of
// the same fetch effect, its own JSX block, AND an entry in two hand-written guards — the
// "still resolving" condition and the "nothing to show" condition. Forgetting either guard entry
// does not fail to compile: it makes the whole Device-health section vanish for every node, which
// reads as "this device reports no health" rather than as a bug.
//
// So the set of cards is a list, the resolution is one function over that list, and the guards are
// computed from its result. Adding a gauge is one entry here plus its two locale strings.
//
// Since ADR-046 Inc.6 it decides for the section *below* Device health too. The node's remaining
// node-level metrics are drawn as the same card, and `overviewScalarCards` is what keeps the two
// sections from showing the same measurement twice — subtracting what Device health already
// claimed, including the two inputs of the derived memory card.
//
// This is a `.ts` file on purpose: Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so logic left in the `.tsx` is logic nothing tests.

import type { MemId } from '../../lib/format';
import {
  overviewScalars,
  viewOf,
  type MetricChartQuery,
  type MetricRead,
} from '../../lib/metricInventory';
import type { NodeMetricEntry } from '../../types/api';

/** How a card's values read, which decides both the headline format and the chart's Y axis. */
export type MetricScale =
  /** A bounded 0–100 gauge (CPU, memory): the chart's baseline is 0, not the window minimum. */
  | 'percent'
  /** An unbounded count (sessions, tunnels, users): the axis auto-fits and uses SI suffixes. */
  | 'count';

/** One Device-health gauge. */
export interface MetricCardSpec {
  /** Stable identity, used as the React key and as the key of the resolution map. */
  readonly id: string;
  /** i18n key in the `nodes` namespace. */
  readonly labelKey: string;
  /** Candidate metric names in priority order; the first one the node collects wins. */
  readonly candidates: readonly string[];
  readonly scale: MetricScale;
  /** Appended to the headline and hover value — `/s` for a rate. */
  readonly unit?: string;
}

export const METRIC_CARDS = [
  {
    // Vendor/host CPU gauges that read 0–100. All three are per-entity tables, collapsed node-wide
    // by the shared `max` rule below.
    id: 'cpu',
    labelKey: 'overview.cpu',
    candidates: ['huawei_cpu_usage', 'cisco_cpu_5min', 'hr_processor_load'],
    scale: 'percent',
  },
  {
    // Current concurrent sessions. Mixed sources: Huawei USG / Cisco ASA are per-entity tables,
    // Fortinet and PAN-OS are scalars.
    id: 'sessions',
    labelKey: 'overview.sessions',
    candidates: [
      'huawei_usg_total_sessions',
      'fortinet_sessions',
      'asa_current_connections',
      'panos_sessions_active',
    ],
    scale: 'count',
  },
  {
    // New-session setup rate. Its own card rather than a second series on the total: a count and a
    // per-second rate on one axis flattens whichever is smaller.
    id: 'setupRate',
    labelKey: 'overview.setupRate',
    // The since-boot counter first: the device's own `setup_rate` column is a one-second
    // instantaneous sample read every five minutes, so on real hardware most samples are 0 and the
    // rest are spikes. The counter is served through rate() and reads as an actual rate (ADR-070).
    candidates: ['huawei_usg_session_total', 'huawei_usg_session_setup_rate'],
    scale: 'count',
    unit: '/s',
  },
  {
    // Remote-access VPN users (AnyConnect / SSL-VPN) on firewalls used as VPN heads.
    id: 'vpnUsers',
    labelKey: 'overview.vpnUsers',
    candidates: ['cisco_ra_sessions', 'fortinet_sslvpn_users'],
    scale: 'count',
  },
  {
    // Site-to-site IPsec/IKE or GlobalProtect tunnels. All scalar sources today.
    id: 'vpnTunnels',
    labelKey: 'overview.vpnTunnels',
    candidates: [
      'cisco_ipsec_active_tunnels',
      'cisco_ike_active_tunnels',
      'fortinet_vpn_tunnels_up',
      'panos_gp_active_tunnels',
    ],
    scale: 'count',
  },
] as const satisfies readonly MetricCardSpec[];

export type MetricCardId = (typeof METRIC_CARDS)[number]['id'];

/**
 * A card resolved against a node's metric inventory: which metric to read, and how to read it.
 *
 * `read` and `chart` come from [`metricView`] rather than from this module. They were an `agg?:
 * 'max'` flag derived from `dimension` alone, which is how the `setupRate` card ended up drawing
 * `huawei_usg_session_total` — a **counter** — as a raw range and printing its since-boot total
 * (18,190,268) as a per-second rate. `dimension` decides whether the rows collapse; only
 * `metric_kind` decides whether the stored value is a measurement or an odometer reading, and
 * nothing here was asking (ADR-046 Inc.6 決定 L; the accident itself is ADR-012's).
 */
export interface ResolvedMetric {
  metric: string;
  /**
   * How to read the current value. `none` for a counter — its stored value is an odometer, so the
   * headline comes from the last point of the rate series instead.
   */
  read: MetricRead;
  /**
   * How to chart it. Never `none` or `interfaces`: [`resolveCard`] refuses a candidate it cannot
   * draw rather than producing a card with an empty chart under it.
   */
  chart: MetricChartQuery;
}

/**
 * Pick the first candidate the node actually has.
 *
 * The source is the metric **inventory** (`listNodeMetrics`), not the collection set. Two things
 * change with that: the inventory needs only read permission, so a viewer sees these cards at all —
 * they were admin-only by accident, since the collection endpoint requires ManageConfig — and its
 * `status` is measured from the series that arrived rather than inferred from the collection set,
 * so a card is offered only when there is something to draw. (`dimension` comes from the
 * collection item and not from the series' row keys — a chassis CPU whose table index happens to
 * collide with an ifIndex is still not a per-interface metric.)
 */
export function resolveCard(
  items: readonly NodeMetricEntry[],
  candidates: readonly string[],
): ResolvedMetric | null {
  for (const metric of candidates) {
    const it = items.find((i) => i.metric === metric);
    // `no_data` entries are configured but silent — offering a card for one draws an empty chart
    // under a headline dash, which reads as a broken widget rather than as a quiet device.
    if (!it || it.status === 'no_data') continue;
    const { read, chart } = viewOf(it);
    // A candidate this surface cannot draw is not a card, and the next candidate down is free to
    // win. Two cells of the table land here: a per-entity counter has no query at all (it would
    // have to be differentiated per row and then collapsed, and a folded multi-index table's rows
    // cannot be named), and a per-interface one belongs to the Interfaces tab, which shows every
    // row by name. Falling through rather than returning a drawable-looking card is the point —
    // the alternative is a headline over a permanently empty chart.
    if (chart.kind === 'none' || chart.kind === 'interfaces') continue;
    return { metric, read, chart };
  }
  return null;
}

/**
 * A memory source. Unlike the cards above, memory needs **two** inputs and derived arithmetic
 * (the per-`id` math lives in `deriveMem`, lib/format), which is why it stays its own component
 * rather than folding into the generic card: it shows an absolute used/total headline over a
 * usage-% trend, not a single gauge value.
 */
export interface MemSpec {
  readonly id: MemId;
  /** The two raw metrics (both required) that derive used+total; also the chart's inputs. */
  readonly metrics: readonly [string, string];
  /** Scale of the metrics to bytes (1 for byte OIDs, 1024 for KB). */
  readonly unitToBytes: number;
}

// Order is precedence: `resolveMem` takes the first source whose **both** inputs the node has.
// The Cisco families are mutually exclusive in practice — ciscoMemoryPool answers on 2960X/3560,
// cempMemPool on Catalyst 9000 / Nexus / IOS-XR / ASA — so the order between them only decides a
// device that somehow answers both, where the 64-bit family is the better answer anyway.
export const MEM_SPECS = [
  { id: 'huawei', metrics: ['huawei_mem_total', 'huawei_mem_free'], unitToBytes: 1 },
  { id: 'cisco', metrics: ['cisco_mem_used', 'cisco_mem_free'], unitToBytes: 1 },
  { id: 'cisco-cemp', metrics: ['cisco_cemp_mem_used', 'cisco_cemp_mem_free'], unitToBytes: 1 },
  // cpmCPUMemory is reported in kilobytes, unlike the two byte-valued families above.
  { id: 'cisco-cpu', metrics: ['cisco_cpu_mem_used', 'cisco_cpu_mem_free'], unitToBytes: 1024 },
  { id: 'ucd', metrics: ['ucd_mem_total_kb', 'ucd_mem_avail_kb'], unitToBytes: 1024 },
] as const satisfies readonly MemSpec[];

/** A memory source resolved against a node's collection set. */
export interface ResolvedMem {
  id: MemId;
  metrics: readonly [string, string];
  unitToBytes: number;
}

/** The first memory source whose **both** inputs the node collects; used+total needs the pair. */
export function resolveMem(items: readonly NodeMetricEntry[]): ResolvedMem | null {
  const names = new Set(items.filter((i) => i.status !== 'no_data').map((i) => i.metric));
  const spec = MEM_SPECS.find((s) => s.metrics.every((m) => names.has(m)));
  return spec ? { id: spec.id, metrics: spec.metrics, unitToBytes: spec.unitToBytes } : null;
}

/** Every card resolved against one node, plus its memory source. */
export interface ResolvedHealth {
  cards: Record<MetricCardId, ResolvedMetric | null>;
  mem: ResolvedMem | null;
}

/**
 * Resolve the whole Device-health section for a node in one pass.
 *
 * Returning a map keyed by [`MetricCardId`] is what makes the two render guards derivable: the
 * section is "still resolving" while this has not returned, and "nothing to show" when
 * [`hasAnyHealth`] is false. Neither is a hand-maintained list of card names any more.
 */
export function resolveHealth(items: readonly NodeMetricEntry[]): ResolvedHealth {
  const cards = {} as Record<MetricCardId, ResolvedMetric | null>;
  for (const spec of METRIC_CARDS) cards[spec.id] = resolveCard(items, spec.candidates);
  return { cards, mem: resolveMem(items) };
}

/** Whether a node has anything at all to show in Device health. */
export function hasAnyHealth(h: ResolvedHealth): boolean {
  return h.mem != null || METRIC_CARDS.some((s) => h.cards[s.id] != null);
}

/**
 * Every metric [`resolveHealth`] has already claimed, so the generic section below can subtract it.
 *
 * ⚠️ **The memory inputs count.** `MEMORY` is derived from a pair (`*_total` + `*_free`) that
 * never appears as a card's `metric`, so counting `cards` alone leaves both raw byte gauges free to
 * reappear underneath the card that is made of them.
 */
export function claimedMetrics(h: ResolvedHealth): Set<string> {
  const out = new Set<string>();
  for (const spec of METRIC_CARDS) {
    const r = h.cards[spec.id];
    if (r) out.add(r.metric);
  }
  if (h.mem) for (const m of h.mem.metrics) out.add(m);
  return out;
}

/** One card in the generic node-level section: which metric, and how it must be read and drawn. */
export interface ScalarCard {
  metric: string;
  read: MetricRead;
  chart: MetricChartQuery;
}

/**
 * The node-level metrics the Overview draws as generic cards, in inventory order.
 *
 * `overviewScalars` decides what belongs on the Overview at all (no counters — they have no
 * glanceable value; no per-interface metrics — eight octet counters above the fold on every switch
 * is what 決定 1's "don't degrade the common case" forbids). This adds the second rule: **anything
 * Device health is already drawing is dropped.** A number beside a chart of the same metric was
 * harmless; two charts of it, stacked, read as a second measurement that happens to always agree.
 *
 * `health === null` means Device health has not resolved yet. Nothing is subtracted then, and the
 * caller should not render — drawing the unsubtracted set first would flash the duplicates.
 */
export function overviewScalarCards(
  entries: readonly NodeMetricEntry[],
  health: ResolvedHealth | null,
): ScalarCard[] {
  const claimed = health ? claimedMetrics(health) : new Set<string>();
  return overviewScalars(entries)
    .filter((e) => !claimed.has(e.metric))
    .map((e) => ({ metric: e.metric, ...viewOf(e) }))
    // The same refusal `resolveCard` applies. Unreachable while `overviewScalars` drops counters
    // and per-interface metrics — kept so that widening *that* predicate cannot silently produce a
    // card with no query behind it, which is the failure this file's whole shape exists to prevent.
    .filter((c) => c.chart.kind !== 'none' && c.chart.kind !== 'interfaces');
}

/**
 * The last real sample in a series, or `null`.
 *
 * This is how a **counter** card gets its headline. `getNodeMetric` would answer with the stored
 * value, which for a counter is the odometer — the reading that made `SETUP RATE` print
 * `18,190,268/s` on a firewall doing a few new sessions a second. There is no "latest rate"
 * endpoint and there should not be one: the rate is defined by the window it was derived over, so
 * the series that was already fetched for the chart is the only thing that knows it.
 */
export function lastValue(values: readonly number[]): number | null {
  for (let i = values.length - 1; i >= 0; i--) {
    const v = values[i];
    if (Number.isFinite(v)) return v;
  }
  return null;
}
