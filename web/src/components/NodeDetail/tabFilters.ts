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

import { isFiltered as isFilteredAgainst, textMatch } from '../../lib/filterQuery';
import type { Neighbor, NeighborProto, NodeMetricEntry } from '../../types/api';

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

export interface NeighborFilters {
  /** Which protocol reported the adjacency. */
  proto: NeighborProto | '';
  /** Free text over the peer's name, the local port and the peer's port. */
  q: string;
}

export const DEFAULT_NEIGHBOR_FILTERS: NeighborFilters = { proto: '', q: '' };

export function matchesNeighbor(n: Neighbor, f: NeighborFilters): boolean {
  if (f.proto && n.proto !== f.proto) return false;
  return textMatch(f.q, n.remote_sys_name, n.local_port, n.remote_port, n.remote_port_desc);
}

export function isNeighborFiltered(f: NeighborFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_NEIGHBOR_FILTERS);
}

// ────────────────────────────────────────────────────────────────── collection

export interface MetricFilters {
  /** Hide the metrics that are configured but not arriving. */
  flowingOnly: boolean;
  /** Free text over the metric name. */
  q: string;
}

export const DEFAULT_METRIC_FILTERS: MetricFilters = { flowingOnly: false, q: '' };

/** Whether one collected metric survives the filter.
 *
 *  "Flowing" is `status === 'ok'` — the store's own answer, which is what the status column shows,
 *  rather than a second opinion derived from whether a last value happens to be present. A metric
 *  that stopped arriving an hour ago still has one. */
export function matchesMetric(m: NodeMetricEntry, f: MetricFilters): boolean {
  if (f.flowingOnly && m.status !== 'ok') return false;
  return textMatch(f.q, m.metric);
}

export function isMetricFiltered(f: MetricFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_METRIC_FILTERS);
}
