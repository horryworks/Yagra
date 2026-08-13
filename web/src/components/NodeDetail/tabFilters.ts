// SPDX-License-Identifier: AGPL-3.0-only
// Which rows three node-detail tabs show — Interfaces, Neighbors and Collection.
//
// Client-side, and here rather than in the tabs because Vitest never executes a `.tsx` (testing.md).
// The three are bounded per node: a device has interfaces, neighbours and metrics in the dozens or
// low hundreds, and the tab already has all of them in hand. They are *not* the fleet-scaling lists
// `ui-conventions` sends to the server — those are the fleet-wide screens.
//
// One module because the three ask the same shape of question and were about to grow three
// near-identical `.toLowerCase().includes()` blocks. The Interfaces tab already had one, hand-rolled
// and searching two fields where the row shows five.

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../../lib/columnFilter';
import { isFiltered as isFilteredAgainst, textMatch } from '../../lib/filterQuery';
import { NEIGHBOR_PROTOS, type Neighbor, type NodeMetricEntry } from '../../types/api';

// ───────────────────────────────────────────────────────────────── interfaces

/** How an interface is running, as the toolbar offers it.
 *
 *  ⚠️ `oper_status` is the SNMP `ifOperStatus` integer, where **1 is up and everything else is
 *  not** — including `null`, which means the poller has never had an answer. The three buckets
 *  below are exhaustive over that, so "up" + "down" + "unknown" is always the whole list. */
export const IF_STATES = ['up', 'down', 'unknown'] as const;
export type IfState = (typeof IF_STATES)[number];

/** An interface's bucket. Shared by the filter and by anything that wants to count them, so the
 *  dropdown and the summary can never disagree about what "up" means. */
export function ifState(operStatus: number | null | undefined): IfState {
  if (operStatus == null) return 'unknown';
  return operStatus === 1 ? 'up' : 'down';
}

/** The fields the interface filter reads — structural, so a test needs no full row. */
export interface FilterableInterface {
  ifindex: number;
  if_name?: string | null;
  if_alias?: string | null;
  oper_status?: number | null;
}

export interface InterfaceFilters {
  state: IfState | '';
  /** Free text over the interface's name and its description. */
  q: string;
}

export const DEFAULT_INTERFACE_FILTERS: InterfaceFilters = { state: '', q: '' };

/** Whether one interface survives the filter.
 *
 *  The name falls back to `if<ifindex>` exactly as the row renders it, so typing what is on screen
 *  finds the row even when the device reports no `ifName`. */
export function matchesInterface(r: FilterableInterface, f: InterfaceFilters): boolean {
  if (f.state && ifState(r.oper_status) !== f.state) return false;
  return textMatch(f.q, r.if_name ?? `if${r.ifindex}`, r.if_alias);
}

export function isInterfaceFiltered(f: InterfaceFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_INTERFACE_FILTERS);
}

// ────────────────────────────────────────────────────────────────── neighbours

/**
 * The Neighbors tab's filter row, keyed by `Column.key` (ADR-053 Inc.3).
 *
 * The search box this replaces read four fields at once — peer name, local port, remote port and
 * remote port description. Split per column that is three controls, and the one that changes
 * meaning is the peer: its cell renders the system name *or* the chassis id, so the column filter
 * reads both. Typing what is on screen has to find the row, whichever of the two is showing.
 */
export function neighborFilters(t: TFunction): Record<string, ColumnFilterSpec<Neighbor>> {
  return {
    local: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (n) => [n.local_port],
      containsSemantics: 'substring',
      placeholder: t('neighbors.colLocalPort'),
    },
    peer: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (n) => [n.remote_sys_name, n.remote_chassis],
      containsSemantics: 'substring',
      placeholder: t('neighbors.colPeer'),
    },
    remote_port: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (n) => [n.remote_port, n.remote_port_desc],
      containsSemantics: 'substring',
      placeholder: t('neighbors.colRemotePort'),
    },
    proto: {
      kind: 'enum',
      options: NEIGHBOR_PROTOS.map((p) => ({ value: p, label: t(`neighbors.proto.${p}`) })),
      readValue: (n) => n.proto,
      allLabel: t('neighbors.colProto'),
      counts: 'client',
    },
  };
}

// ────────────────────────────────────────────────────────────────── collection

export interface MetricFilters {
  /** Hide the metrics that are not arriving. */
  flowingOnly: boolean;
  /** Free text over the metric name. */
  q: string;
}

export const DEFAULT_METRIC_FILTERS: MetricFilters = { flowingOnly: false, q: '' };

/** Whether a metric has samples in the inventory window.
 *
 *  ⚠️ **`ok` alone is the wrong answer, and it is the obvious one.** `MetricStatus` crosses two
 *  facts, not one: `ok` = configured **and** arriving, `no_data` = configured and **not** arriving,
 *  `unconfigured` = **not** configured and arriving. So the metrics that come from no collection
 *  set at all — ICMP reachability, `http_up`, `dns_up`, the neighbour count, values extracted from
 *  a monitored JSON response — are `unconfigured` while flowing perfectly.
 *
 *  Reading "flowing" as `status === 'ok'` therefore hides exactly the live metrics on the kinds of
 *  node that have nothing else: a URL or DNS monitor has no collection set, so the toggle would
 *  empty the tab it is meant to narrow. */
export function metricIsFlowing(m: NodeMetricEntry): boolean {
  return m.status === 'ok' || m.status === 'unconfigured';
}

/** Whether one collected metric survives the filter.
 *
 *  The judgement is the store's own status, rather than a second opinion derived from whether a
 *  last value happens to be present: a metric that stopped arriving an hour ago still has one. */
export function matchesMetric(m: NodeMetricEntry, f: MetricFilters): boolean {
  if (f.flowingOnly && !metricIsFlowing(m)) return false;
  return textMatch(f.q, m.metric);
}

export function isMetricFiltered(f: MetricFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_METRIC_FILTERS);
}
