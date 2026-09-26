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

/** The two views of Nodes ▸ Discovery (ADR-179 決定 9), held in the URL as `?tab=`. `scan` is the
 *  default and is written as no key at all, so every link made before the tabs existed still opens
 *  the sweep form. */
export const DISCOVERY_TABS = ['scan', 'unregistered'] as const;
export type DiscoveryTab = (typeof DISCOVERY_TABS)[number];

type GeneratedSource = DiscoveredEndpoint['evidence'][number]['source'];

/** Where an endpoint can have been seen, in the order the backend lists evidence.
 *
 *  `as const` because the UI builds `discovery.seen.source.<token>` at runtime and
 *  `i18nEnumKeys.test.ts` has to be able to iterate it. The two type checks below pin it to the
 *  generated union in both directions, so a source the backend gains is a compile error here rather
 *  than a raw key on screen. */
export const ENDPOINT_SOURCES = ['arp', 'lldp', 'cdp', 'ospf', 'bgp', 'syslog', 'trap'] as const satisfies readonly GeneratedSource[];
export type EndpointSource = (typeof ENDPOINT_SOURCES)[number];
// Every generated source is listed — the direction `satisfies` cannot check.
const everySourceListed: Exclude<GeneratedSource, EndpointSource> extends never ? true : never = true;
void everySourceListed;

/** The distinct sources that saw one endpoint, in listing order — the row's "seen via" summary. */
export function sourcesOf(e: DiscoveredEndpoint): EndpointSource[] {
  const seen = new Set(e.evidence.map((ev) => ev.source));
  return ENDPOINT_SOURCES.filter((s) => seen.has(s));
}

/** The port the row's own observer named for it (LLDP/CDP), if any. The row's `via_node` is the
 *  lowest observer across every source; this finds that observer's port name, so the cell reads
 *  "sw-01 · Gi1/0/1" rather than an ifIndex whenever a neighbour table supplied one. */
export function portName(e: DiscoveredEndpoint): string | null {
  if (e.via_node == null) return null;
  const hit = e.evidence.find((ev) => ev.via_node === e.via_node && ev.port);
  return hit?.port ?? null;
}

/** The name an import should give the new node: the name a source reported, or nothing (the backend
 *  then uses the address). Whitespace-only counts as nothing. */
export function importName(e: DiscoveredEndpoint): string | undefined {
  const n = e.name?.trim();
  return n ? n : undefined;
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
