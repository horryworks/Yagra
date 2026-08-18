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
// This is a `.ts` file on purpose: Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so logic left in the `.tsx` is logic nothing tests.

import type { MemId } from '../../lib/format';
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

/** A card resolved against a node's metric inventory: which metric to read, and how. */
export interface ResolvedMetric {
  metric: string;
  /**
   * `max` for per-entity sources, collapsing rows (per-CPU, per-VDOM, per-context) to one
   * node-level value at query time; absent for node-level sources, which already are one series.
   */
  agg?: 'max';
}

/**
 * Pick the first candidate the node actually has.
 *
 * The source is the metric **inventory** (`listNodeMetrics`), not the collection set. Two things
 * change with that: the inventory needs only read permission, so a viewer sees these cards at all —
 * they were admin-only by accident, since the collection endpoint requires ManageConfig — and its
 * `dimension` is measured from the series that arrived rather than inferred from the collection
 * kind, so a card is offered when there is something to draw.
 */
export function resolveCard(
  items: readonly NodeMetricEntry[],
  candidates: readonly string[],
): ResolvedMetric | null {
  for (const metric of candidates) {
    const it = items.find((i) => i.metric === metric);
    // `no_data` entries are configured but silent — offering a card for one draws an empty chart
    // under a headline dash, which reads as a broken widget rather than as a quiet device.
    if (it && it.status !== 'no_data') {
      return { metric, agg: it.dimension === 'none' ? undefined : 'max' };
    }
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
