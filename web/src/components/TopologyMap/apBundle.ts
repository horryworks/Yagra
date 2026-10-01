// SPDX-License-Identifier: AGPL-3.0-only
// The judgements behind a bundle of Wi-Fi access points on the network map (ADR-191 Inc.9): the
// state rim's arcs, the counts under the circle, keeping the members' states live, and the
// per-port list the side panel shows. Kept out of the `.tsx` files so Vitest can reach them.

import type { MapLevel, NodeState } from '../../types/api';
import { SEVERITY_ORDER } from '../../lib/nodeState';
import { worstState, type BundleMember, type PlacedNode } from './graphLayout';

/** How many members are in each state, worst first, states with none left out. */
export function stateCounts(members: readonly { state: NodeState }[]): [NodeState, number][] {
  const out: [NodeState, number][] = [];
  for (const s of SEVERITY_ORDER) {
    const n = members.filter((m) => m.state === s).length;
    if (n > 0) out.push([s, n]);
  }
  return out;
}

/** The counts worth saying under the circle: every state but `ok`. Empty means all of them are ok. */
export function troubleCounts(members: readonly { state: NodeState }[]): [NodeState, number][] {
  return stateCounts(members).filter(([s]) => s !== 'ok');
}

/** One arc of the rim: a state, how long it is, and where it starts, along a circle `circumference`
 *  long. The arcs follow one another worst first, and their lengths add up to the circumference. */
export interface RimArc {
  state: NodeState;
  length: number;
  offset: number;
}

export function rimArcs(members: readonly { state: NodeState }[], circumference: number): RimArc[] {
  const total = members.length;
  if (total === 0) return [];
  const out: RimArc[] = [];
  let offset = 0;
  for (const [state, n] of stateCounts(members)) {
    const length = (circumference * n) / total;
    out.push({ state, length, offset });
    offset += length;
  }
  return out;
}

/**
 * The live state stream applied to the members of every bundle, each bundle recoloured with its
 * worst member. `live` is keyed by box id (`liveByGraphId`). Hands back `nodes` itself when no
 * bundle changed, so a state change elsewhere does not produce a new array.
 */
export function overlayBundleStates(
  nodes: PlacedNode[],
  live: ReadonlyMap<string, NodeState>,
): PlacedNode[] {
  let changed = false;
  const out = nodes.map((n) => {
    if (!n.bundle) return n;
    let moved = false;
    const members: BundleMember[] = n.bundle.members.map((m) => {
      const s = live.get(m.id);
      if (s === undefined || s === m.state) return m;
      moved = true;
      return { ...m, state: s };
    });
    if (!moved) return n;
    changed = true;
    return { ...n, state: worstState(members.map((m) => m.state)), bundle: { ...n.bundle, members } };
  });
  return changed ? out : nodes;
}

/** One access point in the side panel's list. `id` is the server's node id. */
export interface PortMember {
  id: string;
  name: string;
  state: NodeState;
}

/** The access points that reach the parent through one of its ports. `port` is null when the link
 *  names no port on the parent's side. */
export interface PortGroup {
  port: string | null;
  members: PortMember[];
}

/** The server's node id behind a box id, for a node box. */
function nodeIdOf(boxId: string): string {
  return boxId.startsWith('node:') ? boxId.slice('node:'.length) : boxId;
}

/**
 * A bundle's members grouped by the parent's port they are cabled to, read from the level's lines.
 * Ports holding a member in trouble come first, then the busiest ports, then by name; inside a
 * port, the worst state first, then by name. Several access points on one port usually means a
 * switch nobody monitors sits between them and the parent.
 */
export function membersByPort(level: MapLevel, bundle: NonNullable<PlacedNode['bundle']>): PortGroup[] {
  const parent = nodeIdOf(bundle.parent);
  const byPort = new Map<string | null, PortMember[]>();
  for (const m of bundle.members) {
    const id = nodeIdOf(m.id);
    let port: string | null = null;
    for (const e of level.edges) {
      const aIsParent = e.a.kind === 'node' && e.a.id === parent && e.b.id === id;
      const bIsParent = e.b.kind === 'node' && e.b.id === parent && e.a.id === id;
      if (!aIsParent && !bIsParent) continue;
      const first = e.members[0];
      port = (aIsParent ? first?.a_if_name : first?.b_if_name) || null;
      break;
    }
    if (!byPort.has(port)) byPort.set(port, []);
    byPort.get(port)!.push({ id, name: m.name, state: m.state });
  }
  const rank = (s: NodeState) => SEVERITY_ORDER.indexOf(s);
  const groups = [...byPort].map(([port, members]) => ({
    port,
    members: members.sort((x, y) => rank(x.state) - rank(y.state) || (x.name < y.name ? -1 : x.name > y.name ? 1 : 0)),
  }));
  const troubled = (g: PortGroup) => g.members.some((m) => m.state !== 'ok');
  return groups.sort(
    (x, y) =>
      Number(troubled(y)) - Number(troubled(x)) ||
      y.members.length - x.members.length ||
      portOrder(x.port, y.port),
  );
}


/** Ports by name, a member with no port named last. */
function portOrder(x: string | null, y: string | null): number {
  if (x === y) return 0;
  if (x === null) return 1;
  if (y === null) return -1;
  return x.localeCompare(y);
}
