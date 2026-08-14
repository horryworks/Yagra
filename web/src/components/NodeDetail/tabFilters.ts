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
import {
  specColumns,
  type ColumnFilterSpec,
  type FilterableColumn,
} from '../../lib/columnFilter';
import {
  METRIC_STATUSES,
  NEIGHBOR_PROTOS,
  type Neighbor,
  type NodeMetricEntry,
} from '../../types/api';

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

/**
 * The Interfaces tab's filter row, keyed by the column it sits under (ADR-053 Inc.6 decision F).
 *
 * The tab keeps its own grid — the rows are `<button>`s that drive the detail dock below, which is
 * not a shape `DataTable` has — so what moved is the controls, not the table. The one-box search
 * this replaces read the name and the description together, where the two are separate columns on
 * screen; splitting them is what lets "port 3 of anything" and "the uplink description" be
 * different questions.
 *
 * ⚠️ The name falls back to `if<ifindex>` exactly as the row renders it, so typing what is on screen
 * finds the row even when the device reports no `ifName`.
 */
export function interfaceFilters(
  t: TFunction,
): Record<string, ColumnFilterSpec<FilterableInterface>> {
  return {
    if_name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (r) => [r.if_name ?? `if${r.ifindex}`],
      containsSemantics: 'substring',
      placeholder: t('interfaces.colInterface'),
    },
    if_alias: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (r) => [r.if_alias],
      containsSemantics: 'substring',
      placeholder: t('interfaces.colDescription'),
    },
    oper: {
      kind: 'enum',
      options: IF_STATES.map((s) => ({ value: s, label: t(`interfaces.state.${s}`) })),
      // Through `ifState`, so the control and the row's dot can never disagree about what "up"
      // means — `oper_status` is an integer where 1 is up and `null` is "never answered".
      readValue: (r) => ifState(r.oper_status),
      allLabel: t('interfaces.allStates'),
      counts: 'client',
    },
  };
}

export function interfaceColumns(t: TFunction): FilterableColumn<FilterableInterface>[] {
  return specColumns(interfaceFilters(t));
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

/**
 * The Collection tab's all-metrics filter row (ADR-053 Inc.6 decision F).
 *
 * ⚠️ **The "flowing only" checkbox became a `status` set, and that is a widening, not a rename.**
 * The checkbox could say "hide what is not arriving" and nothing else; the column it now sits under
 * renders three distinct statuses, and "show me only the configured-but-silent ones" — the actual
 * question when a collection set stops working — was unsayable. `metricIsFlowing` survives because
 * two other places ask it; see its own note on why `ok` alone is the wrong reading.
 *
 * The judgement is the store's own status, rather than a second opinion derived from whether a last
 * value happens to be present: a metric that stopped arriving an hour ago still has one.
 */
export function metricFilters(t: TFunction): Record<string, ColumnFilterSpec<NodeMetricEntry>> {
  return {
    metric: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (m) => [m.metric],
      containsSemantics: 'substring',
      placeholder: t('collection.colMetric'),
    },
    status: {
      kind: 'enum',
      options: METRIC_STATUSES.map((s) => ({ value: s, label: t(`collection.status.${s}`) })),
      readValue: (m) => m.status,
      allLabel: t('collection.filter.allStatuses'),
      counts: 'client',
    },
  };
}

export function metricColumns(t: TFunction): FilterableColumn<NodeMetricEntry>[] {
  return specColumns(metricFilters(t));
}
