// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the node-detail Overview's facts grid shows, and whether it charts ICMP RTT — both
// decided by the node's kind.
//
// Lives in a `.ts` next to the tab rather than inside `OverviewTab.tsx` because Vitest runs with
// `include: ['src/**/*.test.ts']` — a test in a `.tsx` is a file nothing executes (`testing.md`).
//
// The rows dropped for a monitor are not "usually blank", they are structurally absent: a URL or
// DNS monitor is never given an SNMP credential to bind (`scheduler/assemble.rs::assemble_node_jobs` returns
// one HTTP/DNS job before the SNMP branch), so Maker, Model, SNMP credential and Uptime — which all
// come from an SNMP walk — can only ever render an em dash. Printing four dashes in a row tells an
// operator "this device is not answering" about something that was never asked.

import { NODE_KINDS, type NodeKind } from '../../types/api';

/** Every fact row, in the order the grid lays them out. The single enumeration; per-kind lists are
 *  membership only, never order (see [`visibleFactRows`]). */
export const FACT_ROWS = [
  'group',
  'pool',
  'polledBy',
  'address',
  'maker',
  'model',
  'profile',
  'credential',
  'parent',
  'uptime',
  'resolver',
] as const;

export type FactRow = (typeof FACT_ROWS)[number];

/** Rows every kind shows. "Where is this filed, and who polls it" is true of a URL monitor exactly
 *  as it is of a switch — a monitor is assigned to a poller pool like anything else. */
const PLACEMENT: readonly FactRow[] = ['group', 'pool', 'polledBy', 'profile', 'parent'];

/** Rows that come from an SNMP walk of the node itself, plus its management address. Only an
 *  ordinary device has them; a Meraki node keeps the descriptive ones because the org collector
 *  reports maker/model and the node has a real management address, but not `uptime` (no sysUpTime
 *  walk) — see [`FACT_ROWS_BY_KIND`]. */
const DEVICE_IDENTITY: readonly FactRow[] = ['address', 'maker', 'model', 'credential'];

export const FACT_ROWS_BY_KIND: Record<NodeKind, readonly FactRow[]> = {
  device: [...PLACEMENT, ...DEVICE_IDENTITY, 'uptime'],
  meraki: [...PLACEMENT, ...DEVICE_IDENTITY],
  // The URL itself is the header's sub line, so it is not repeated here.
  url: PLACEMENT,
  // A DNS monitor's `address` is its resolver, or 0.0.0.0 when it uses the system one — so it gets
  // a named Resolver row instead of an "IP address" row that means something else.
  dns: [...PLACEMENT, 'resolver'],
};

/** The rows this kind shows, in `FACT_ROWS` order. Derived from the registry so the grid's reading
 *  order cannot drift between kinds. */
export function visibleFactRows(kind: NodeKind): readonly FactRow[] {
  const allowed = FACT_ROWS_BY_KIND[kind];
  return FACT_ROWS.filter((row) => allowed.includes(row));
}

/** Whether the Overview leads with the ICMP RTT sparkline.
 *
 *  Only an ordinary device. `scheduler/assemble.rs::assemble_node_jobs` returns before the ICMP branch for a
 *  URL or DNS monitor (a URL target may be unpingable behind a CDN, and a name has no address of
 *  its own), and a Meraki node emits no per-node job at all. `icmp_rtt_ms` is therefore absent by
 *  construction, and "No RTT history yet…" reads as a fault rather than as "not applicable". */
export function overviewShowsIcmp(kind: NodeKind): boolean {
  return kind === 'device';
}

/** Compile-time proof that the per-kind lists name only real rows. */
const _rowsAreRows: readonly (readonly FactRow[])[] = NODE_KINDS.map((k) => FACT_ROWS_BY_KIND[k]);
void _rowsAreRows;
