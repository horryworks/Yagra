// SPDX-License-Identifier: AGPL-3.0-only
// The judgements behind one level of the network map (ADR-191), kept out of the `.tsx` files so
// Vitest can reach them: turning the server's level into boxes and lines, what the URL's `sel=`
// selects, where a stub leads, where a node sits, and what one Escape press clears.

import type { MapEdge, MapLevel, MapNode, MapStub, NodeState } from '../../types/api';
import type { TreeSelection } from '../NodeTree/NodeTree';
import { worstStateFromCounts } from '../../dashboard/widgets/util';
import { nodesPageHref, topologyMapHref } from '../../lib/entityHref';
import type { GraphInput, GraphNodeKind } from './graphLayout';

/** A box's id on the canvas. Kinds are prefixed because a node, a folder and a stub are separate
 *  things that must never share a box, whatever their ids. */
export function graphId(kind: GraphNodeKind, id: string): string {
  return `${kind}:${id}`;
}

/** The two per-kind captions a box carries under its name, supplied by the caller (they are
 *  translated text). */
export interface LevelCaptions {
  folder: (f: MapLevel['folders'][number]) => string;
  stub: (s: MapStub) => string;
}

/** How a folder path is joined, on a box and in the side panel alike. */
const PATH_SEP = ' › ';

/** The folders between a flat level and a node, outermost first; empty for a node directly on it. */
function folderTrail(node: MapNode): string[] {
  return node.folder_path.map((g) => g.name);
}

/** Where a node sits, from the top of the tree: the level's ancestors, the level itself, and — on a
 *  level drawn flat — the folders down to the one the node is filed in. */
export function nodePlace(level: MapLevel, node: MapNode, levelName: string): string {
  return [...level.breadcrumbs.map((b) => b.name), levelName, ...folderTrail(node)].join(PATH_SEP);
}

/** The server's level as the layout's input: one box per node, folder and stub, one line per
 *  bundled edge. Stubs carry no state colour of their own, so they are drawn `unknown`. On a level
 *  drawn flat (inside a site), a node filed in a subfolder carries the folders down to its own as
 *  its second line. */
export function levelToGraph(level: MapLevel, captions: LevelCaptions): GraphInput {
  const nodes: GraphInput['nodes'] = [
    ...level.nodes.map((n) => ({
      id: graphId('node', n.id),
      kind: 'node' as const,
      name: n.name,
      state: n.state,
      sub: n.folder_path.length > 0 ? `▤ ${folderTrail(n).join(PATH_SEP)}` : null,
      rootCause: n.root_cause ?? null,
      ap: n.access_point,
    })),
    ...level.folders.map((f) => ({
      id: graphId('folder', f.id),
      kind: 'folder' as const,
      name: f.name,
      state: worstStateFromCounts(f.counts),
      sub: captions.folder(f),
      rootCause: null,
      ap: false,
    })),
    ...level.stubs.map((s) => ({
      id: graphId('external', s.id),
      kind: 'external' as const,
      name: s.name,
      state: 'unknown' as NodeState,
      sub: captions.stub(s),
      rootCause: null,
      ap: false,
    })),
  ];
  const links = level.edges.map((e) => ({
    id: e.id,
    a: graphId(e.a.kind, e.a.id),
    b: graphId(e.b.kind, e.b.id),
    source: e.source,
    count: e.count,
  }));
  return { nodes, links };
}

/** The live state stream keyed the way the canvas keys its boxes — for the level's own nodes only,
 *  so a state change elsewhere in the fleet produces a map with the same contents. */
export function liveByGraphId(
  level: MapLevel | null,
  live: ReadonlyMap<string, NodeState>,
): Map<string, NodeState> {
  const out = new Map<string, NodeState>();
  for (const n of level?.nodes ?? []) {
    const s = live.get(n.id);
    if (s !== undefined) out.set(graphId('node', n.id), s);
  }
  return out;
}

/** What is selected on the map: a node or folder (kept in the URL), or a bundled edge (kept in the
 *  page only — an edge id names two ends of one level and is meaningless on any other). */
export type MapSelection =
  | { kind: 'node'; id: string }
  | { kind: 'folder'; id: string }
  | { kind: 'edge'; id: string }
  | null;

/** The URL's `sel=` (the Nodes page's spelling) as a map selection. */
export function selectionFromParam(sel: TreeSelection): MapSelection {
  if (!sel) return null;
  return sel.kind === 'node' ? { kind: 'node', id: sel.id } : { kind: 'folder', id: sel.id };
}

/** The canvas id of the selected box, or null (nothing, or an edge). */
export function selectedGraphId(sel: MapSelection): string | null {
  if (!sel || sel.kind === 'edge') return null;
  return graphId(sel.kind, sel.id);
}

/** Every bundled edge touching one of the level's nodes. */
export function edgesOf(level: MapLevel, nodeId: string): MapEdge[] {
  return level.edges.filter(
    (e) => (e.a.kind === 'node' && e.a.id === nodeId) || (e.b.kind === 'node' && e.b.id === nodeId),
  );
}

/** Where a stub leads: the first level on which both ends are visible, with the far end selected. */
export function stubHref(stub: MapStub): string {
  return topologyMapHref({
    group: stub.level_group ?? null,
    sel: { kind: stub.kind === 'node' ? 'node' : 'group', id: stub.id },
  });
}

/** The folder a box enters. */
export function folderHref(folderId: string): string {
  return topologyMapHref({ group: folderId });
}

/** The Nodes page on this level's folder, or the whole inventory for the whole network. */
export function levelNodesHref(level: MapLevel): string {
  return nodesPageHref(level.group ? { kind: 'group', id: level.group.id } : null);
}

/** How many observations the last derivation run declined to turn into a link (ADR-191 decision 15).
 *  Showing the total is what keeps an incomplete graph from reading as a complete one — and
 *  `unmatched_lldp_rows` is the number that moves the day a switch that speaks LLDP is racked. */
export function unresolvedCount(level: MapLevel): number {
  const s = level.summary;
  return (
    (s.unmatched_lldp_rows ?? 0) +
    (s.unmatched_cdp_rows ?? 0) +
    (s.ambiguous_mgmt_addrs ?? 0) +
    (s.oversized_segments ?? 0)
  );
}

/** Whether a response belongs to the level the URL asks for. `usePolled` keeps the previous
 *  level's answer until the new one arrives, and drawing it under the new breadcrumb would be a
 *  wrong map, not a slow one. */
export function isLevelFor(level: MapLevel | null, group: string | null): level is MapLevel {
  if (!level) return false;
  return (level.group?.id ?? null) === group;
}

/** What one Escape press clears: the edge first (the narrower, page-only selection), then the
 *  URL's selection. */
export function mapEscapeTarget(
  edgeSelected: boolean,
  urlSelected: boolean,
): 'edge' | 'selection' | null {
  if (edgeSelected) return 'edge';
  if (urlSelected) return 'selection';
  return null;
}

/** A bundle member's two ports, oriented as the edge is (`a` side first). */
export function memberPorts(
  m: MapEdge['members'][number],
  noPort: string,
): { a: string; b: string } {
  return { a: m.a_if_name || noPort, b: m.b_if_name || noPort };
}

/** Roughly how many characters fit on one line of a box, at the map's small font. */
const CHAR_PX = 7;

/** A label cut to fit a box `width` px wide, with `inset` px taken by the accent and the dot. The
 *  full text goes in the box's title, so nothing is lost; the canvas only has room for a prefix. */
export function fitLabel(text: string, width: number, inset: number): string {
  const max = Math.max(4, Math.floor((width - inset - 8) / CHAR_PX));
  return text.length <= max ? text : `${text.slice(0, max - 1)}…`;
}

/** Enter or Space on a focused box or chip does what a click does. */
export function activateOnKey(
  e: { key: string; preventDefault: () => void },
  act: () => void,
): void {
  if (e.key === 'Enter' || e.key === ' ') {
    e.preventDefault();
    act();
  }
}

/** The way back up: the whole network, each ancestor, then this level (not a link). */
export function levelTrail(level: MapLevel, rootLabel: string): { label: string; to?: string }[] {
  const out: { label: string; to?: string }[] = [
    { label: rootLabel, to: level.group ? topologyMapHref({}) : undefined },
  ];
  for (const b of level.breadcrumbs) out.push({ label: b.name, to: folderHref(b.id) });
  if (level.group) out.push({ label: level.group.name });
  return out;
}

/** A box id back into its kind and the server's id. */
export function splitGraphId(id: string): { kind: GraphNodeKind; id: string } | null {
  const sep = id.indexOf(':');
  if (sep <= 0) return null;
  const kind = id.slice(0, sep);
  if (kind !== 'node' && kind !== 'folder' && kind !== 'external') return null;
  return { kind, id: id.slice(sep + 1) };
}

/** Whether a line shows its count chip: when it bundles several links, or when an end is a box
 *  (a folder or a stub), where "how many" is the question the operator has. */
export function edgeShowsChip(edgeId: string, count: number): boolean {
  if (count > 1) return true;
  const [a = '', b = ''] = edgeId.split('|');
  return !a.startsWith('node:') || !b.startsWith('node:');
}

/** Where pressing a box in a folder pane's map takes the Nodes page (ADR-191 Inc.2): the box's node
 *  or folder becomes the tree's selection. A stub selects what it stands for — the far node, or the
 *  folder holding the far end. The map in the pane never descends on its own. */
export function groupMapTarget(boxId: string, level: MapLevel | null): TreeSelection {
  const ref = splitGraphId(boxId);
  if (!ref) return null;
  if (ref.kind === 'node') return { kind: 'node', id: ref.id };
  if (ref.kind === 'folder') return { kind: 'group', id: ref.id };
  const stub = level?.stubs.find((s) => s.id === ref.id);
  if (!stub) return null;
  return { kind: stub.kind === 'node' ? 'node' : 'group', id: stub.id };
}

/** Whether a wheel turn zooms the map. A map inside a scrolling pane leaves the plain wheel to the
 *  page and zooms only with Ctrl (⌘ on a Mac) held; a trackpad pinch arrives as a wheel with Ctrl
 *  set, so it zooms too. */
export function wheelZooms(
  e: { ctrlKey: boolean; metaKey: boolean },
  needsModifier: boolean,
): boolean {
  return !needsModifier || e.ctrlKey || e.metaKey;
}
