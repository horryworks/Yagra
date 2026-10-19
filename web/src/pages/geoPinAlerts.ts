// SPDX-License-Identifier: AGPL-3.0-only
// Which alerting nodes sit under a Geo map pin (ADR-191 Inc.15). The pin's bubble lists them.
//
// The alerts are already in the browser: the shell holds every active alert, fed over SSE. What
// an alert does not carry is the folder its node is filed in, so the page asks
// `POST /api/v1/node-names` for exactly the alerting nodes and gets each one's `group_id` back.
// From a folder to a pin is the server's geo inheritance (`geo_group`), the same fold the pin's
// colour uses — so a node counted in a pin's colour is the node listed in its bubble.
//
// Pure, so a test reaches it (Vitest does not run `.tsx`).

import { alertSubject, type HasSubject } from '../lib/alertSubject';
import { DISPLAY_ORDER } from '../lib/nodeState';
import type { NodeState } from '../types/api';

/** How many alerting nodes the bubble lists before it says "+N more". */
export const PIN_ALERT_ROWS = 5;

/** The largest batch `POST /api/v1/node-names` reads (`NODE_NAMES_BATCH_MAX` in core, which drops
 *  the tail past it silently). `the_geo_bubbles_batch_is_the_node_names_cap` pins the two; keep
 *  this on one line. */
export const NODE_NAMES_BATCH = 1000;

/** The node ids behind node-subject alerts, once each, in the order the alerts come. A pool's or
 *  a Meraki organization's alert belongs to no site and is left out. */
export function alertNodeIds(alerts: readonly HasSubject[]): string[] {
  const seen = new Set<string>();
  for (const a of alerts) {
    const s = alertSubject(a);
    if (s.kind === 'node' && s.nodeId) seen.add(s.nodeId);
  }
  return [...seen];
}

/** `ids` cut into the batches one `node-names` request accepts. */
export function nodeNameBatches(ids: readonly string[]): string[][] {
  const out: string[][] = [];
  for (let i = 0; i < ids.length; i += NODE_NAMES_BATCH) out.push(ids.slice(i, i + NODE_NAMES_BATCH));
  return out;
}

/** What `node-names` told us about one node. `group` is `null` for an ungrouped node. */
export interface NodePlace {
  name: string;
  group: string | null;
}

/** Read a `node-names` answer into places, or `null` when the core is too old to send
 *  `group_id` — that core cannot say where any node is, and "no alerts here" would be a lie. */
export function placesFromNames(
  entries: readonly { id: string; name: string; group_id?: string | null }[],
): Map<string, NodePlace> | null {
  if (entries.length > 0 && !entries.some((e) => 'group_id' in e)) return null;
  return new Map(entries.map((e) => [e.id, { name: e.name, group: e.group_id ?? null }]));
}

/** One line of the bubble: a node, its worst alert, and how many alerts it has. */
export interface PinAlertRow<A> {
  nodeId: string;
  name: string;
  worst: A;
  count: number;
}

/**
 * The alerting nodes under `pinId`, one row per node, in the order of `alerts`.
 *
 * `alerts` must already be worst-first (`sortedAlerts`), so a node's first alert is its worst and
 * the rows come out worst-first too. `pinOf` maps a folder id to the pin it resolves to
 * (`geo_group`). A node with no known place — not yet answered, outside the caller's scope, or
 * deleted — is left out rather than guessed.
 */
export function pinAlertRows<A extends HasSubject>(
  alerts: readonly A[],
  places: ReadonlyMap<string, NodePlace>,
  pinOf: ReadonlyMap<string, string>,
  pinId: string,
): PinAlertRow<A>[] {
  const rows = new Map<string, PinAlertRow<A>>();
  for (const a of alerts) {
    const s = alertSubject(a);
    if (s.kind !== 'node' || !s.nodeId) continue;
    const place = places.get(s.nodeId);
    if (!place?.group || pinOf.get(place.group) !== pinId) continue;
    const row = rows.get(s.nodeId);
    if (row) row.count += 1;
    else rows.set(s.nodeId, { nodeId: s.nodeId, name: place.name, worst: a, count: 1 });
  }
  return [...rows.values()];
}

/** The states present under a pin, each with its node count, in the legend's order. The bubble
 *  shows these rather than the worst state beside the total: "Unknown · 7 nodes" read as all seven
 *  being unknown when three were. */
export function stateBreakdown(
  counts: Readonly<Record<NodeState, number>> | undefined,
): { state: NodeState; count: number }[] {
  if (!counts) return [];
  return DISPLAY_ORDER.filter((s) => (counts[s] ?? 0) > 0).map((state) => ({
    state,
    count: counts[state],
  }));
}
