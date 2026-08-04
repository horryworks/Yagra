// SPDX-License-Identifier: AGPL-3.0-only
// Judgement for the Discovery ▸ Seen on the network card (ADR-043 Increment 3).
//
// In a .ts, not the .tsx, because Vitest only runs `src/**/*.test.ts` — a test written in a .tsx is
// a file nothing runs (testing.md).

import type { DiscoveredEndpoint, DiscoveredEndpointPage } from '../types/api';

/** How much of the fleet the endpoint list actually speaks for.
 *
 *  `as const` because the UI builds `t()` keys from it at runtime (extensibility §4);
 *  `i18nEnumKeys.test.ts` is what proves every member has strings in both locales. */
export const ENDPOINT_COVERAGE = ['off', 'sampled', 'complete'] as const;
export type EndpointCoverage = (typeof ENDPOINT_COVERAGE)[number];

/** Which of the three the current summary describes.
 *
 *  **This is the whole reason this file exists.** "ARP discovery is switched off" and "ARP discovery
 *  is on and found nothing unmonitored" both render as an empty list, and they mean opposite things:
 *  the second is a clean bill of health, the first is nobody having looked. Telling an operator there
 *  is nothing unmonitored on their network when the walk was never issued is the kind of confident
 *  wrongness the caps and flags elsewhere in this codebase exist to declare rather than smooth over.
 *
 *  `sampled` is the same idea one level down: a router whose ARP cache exceeded its row budget
 *  contributed a sample, so the list is a floor, not a total.
 */
export function coverageOf(summary: DiscoveredEndpointPage['summary']): EndpointCoverage {
  if (!summary || summary.nodes_reporting <= 0) return 'off';
  if (summary.truncated_nodes > 0) return 'sampled';
  return 'complete';
}

/** One port's worth of unmonitored endpoints — the "42 behind Gi0/3" rollup.
 *
 *  Computed from the page in hand rather than read from the API's per-interface counts on purpose:
 *  those counts are *every* endpoint the router resolved, monitored ones included, and the question
 *  this card answers is how many are **not** monitored. Two different numbers; naming them the same
 *  would be the drift trap this repo keeps paying for.
 */
export interface PortRollup {
  viaNode: string | null;
  ifindex: number | null;
  count: number;
}

/** Group a page's endpoints by the port they were seen on, busiest first.
 *
 *  Ties break on `(viaNode, ifindex)` so the order does not shuffle between refreshes — a list that
 *  reorders under a cursor is one an operator cannot scan.
 */
export function rollupByPort(endpoints: DiscoveredEndpoint[]): PortRollup[] {
  const byKey = new Map<string, PortRollup>();
  for (const e of endpoints) {
    const viaNode = e.via_node ?? null;
    const ifindex = e.via_ifindex ?? null;
    const key = `${viaNode ?? '-'}|${ifindex ?? '-'}`;
    const held = byKey.get(key);
    if (held) held.count += 1;
    else byKey.set(key, { viaNode, ifindex, count: 1 });
  }
  return [...byKey.values()].sort(
    (a, b) =>
      b.count - a.count ||
      (a.viaNode ?? '').localeCompare(b.viaNode ?? '') ||
      (a.ifindex ?? -1) - (b.ifindex ?? -1),
  );
}

/** Whether a row is still an unmonitored endpoint, or has since become a node.
 *
 *  The list asks the server for unpromoted rows by default, so this is what keeps the *rendered*
 *  answer honest after an import lands: the row is still on screen until the next fetch, and showing
 *  it as unmonitored would invite the operator to import it twice.
 */
export function isUnmonitored(e: DiscoveredEndpoint): boolean {
  return e.promoted_node_id == null;
}
