// SPDX-License-Identifier: AGPL-3.0-only
// Pure helpers for the inventory tree: build a nested group/node structure from the flat API
// shapes, answer "is X a descendant of Y" for drag-drop cycle guards, and roll up the health of a
// group's descendant nodes. Kept free of React so they can be unit-tested directly.

import type { TFunction } from 'i18next';
import { GROUP_TYPES } from '../types/api';
import type { GroupType, NodeGroup, NodeState, NodeSummary } from '../types/api';
import { pushInto } from './mapBucket';
import { DISPLAY_ORDER, PROBLEM_STATES, emptyStateCounts } from './nodeState';

/** Re-exported for the health-bar/legend call sites that read "the order states are shown in".
 *  The definition lives in `nodeState.ts` with the rest of the NodeState vocabulary. */
export { DISPLAY_ORDER as STATE_ORDER } from './nodeState';

/** A group with its child groups and member nodes resolved (built from the flat lists). */
export interface TreeGroup extends NodeGroup {
  children: TreeGroup[];
  nodes: NodeSummary[];
}

/** The assembled tree: top-level groups + the nodes that belong to no group. */
export interface NodeTreeData {
  roots: TreeGroup[];
  ungrouped: NodeSummary[];
}

/** One collator, built once and reused (ADR-133).
 *
 *  `a.localeCompare(b)` has to obtain a collator for every comparison, and `buildNodeTree` re-sorts
 *  every folder's members from scratch each time a `/nodes/by-group` batch arrives — which, browsing
 *  a thousand folders, is a couple of dozen times.
 *
 *  🚨 **No options, deliberately.** A bare `new Intl.Collator()` performs the same comparison a bare
 *  `localeCompare()` does; passing `numeric: true` or a `sensitivity` here would quietly re-order
 *  every operator's tree, which is not a change a performance fix is allowed to make.
 *  `the_collator_orders_names_exactly_as_localeCompare_does` pins that. */
const COLLATOR = new Intl.Collator();

/** Order siblings by their manual `sort_order` (drag-reorder), falling back to name so equal or
 *  unset orders stay stable. */
const byOrder = <T extends { sort_order: number; name: string }>(a: T, b: T) =>
  a.sort_order - b.sort_order || COLLATOR.compare(a.name, b.name);

/** Build the nested tree from the flat group + node lists. Nodes whose `group_id` is null or
 *  points at an unknown group fall into `ungrouped`; groups whose `parent_id` is unknown are
 *  treated as top-level (so a stale reference can never hide a row). Children and nodes are
 *  ordered by their manual `sort_order` (then name) for stable display. */
export function buildNodeTree(groups: NodeGroup[], nodes: NodeSummary[]): NodeTreeData {
  const byId = new Map<string, TreeGroup>();
  for (const g of groups) byId.set(g.id, { ...g, children: [], nodes: [] });

  const roots: TreeGroup[] = [];
  for (const g of byId.values()) {
    const parent = g.parent_id ? byId.get(g.parent_id) : undefined;
    if (parent) parent.children.push(g);
    else roots.push(g);
  }

  const ungrouped: NodeSummary[] = [];
  for (const n of nodes) {
    const group = n.group_id ? byId.get(n.group_id) : undefined;
    if (group) group.nodes.push(n);
    else ungrouped.push(n);
  }

  for (const g of byId.values()) {
    g.children.sort(byOrder);
    g.nodes.sort(byOrder);
  }
  roots.sort(byOrder);
  ungrouped.sort(byOrder);
  return { roots, ungrouped };
}

/** Every member node at or below a group in the tree — the group's own nodes plus those of all
 *  descendant groups (recursively). Used for the per-group health rollup and member counts. The
 *  passed `group` is a built `TreeGroup` (children + nodes already resolved). */
export function descendantNodes(group: TreeGroup): NodeSummary[] {
  const out: NodeSummary[] = [...group.nodes];
  for (const child of group.children) out.push(...descendantNodes(child));
  return out;
}

/** One visible row of the inventory tree, flattened for virtualized rendering (S13). The tree shape
 *  is carried by row order + `depth` (indentation is purely `depth × INDENT`), so a windowed list of
 *  these renders identically to the old recursive DOM but only builds the on-screen rows. */
export type FlatRow =
  | {
      kind: 'group';
      depth: number;
      group: TreeGroup;
      isOpen: boolean;
      hasChildren: boolean;
      /** Rolled-up health of the group's whole subtree — from the server per-group counts when
       *  supplied (A-3 lazy load, correct over the whole fleet even before members load), else from
       *  the loaded descendant members. Drives the row's health bar + member count.
       *
       *  🚨 **`null` means "not answered yet", and it is not a zero tally** (ADR-133). While
       *  `/fleet/group-summary` is still in flight the row knows nothing about its own membership,
       *  and an empty bar beside a `0` states something false — an operator reads it as "this
       *  folder is empty", which is the one thing the count exists to rule out. A `StateTally | null`
       *  rather than a `countsKnown` flag beside it, so the render site cannot reach for `.total`
       *  without deciding what an unknown count looks like.
       *
       *  Only the browse path can be `null`: narrowing tallies the rows on screen and the legacy
       *  full-node path tallies what it loaded, so both always have an answer of their own. */
      tally: StateTally | null;
    }
  | { kind: 'node'; depth: number; node: NodeSummary }
  /** Placeholder under an open group whose members haven't been lazily fetched yet (A-3). */
  | { kind: 'group-loading'; depth: number; groupId: string }
  /** Under an open group whose member fetch FAILED (ADR-125). A separate variant rather than a
   *  `failed` flag on `group-loading`, so the exhaustive switches — `flatRowKey` here and
   *  `renderRow` in NodeTree.tsx — refuse to compile until someone decides how it looks. Without
   *  it a failed folder is indistinguishable from a slow one and reads as "loading" forever,
   *  which is ADR-055 R6: the screen must say what it cannot do, where the operator is looking. */
  | { kind: 'group-failed'; depth: number; groupId: string }
  | { kind: 'ungrouped-head'; count: number }
  | { kind: 'ungrouped-node'; depth: number; node: NodeSummary };

/** A stable key for a flat row (for React keys + virtualizer identity). */
export function flatRowKey(row: FlatRow): string {
  switch (row.kind) {
    case 'group':
      return `g:${row.group.id}`;
    case 'node':
    case 'ungrouped-node':
      return `n:${row.node.id}`;
    case 'group-loading':
      return `loading:${row.groupId}`;
    case 'group-failed':
      return `failed:${row.groupId}`;
    case 'ungrouped-head':
      return 'ungrouped-head';
  }
}

/** The inventory filter's comparison form: trimmed and lower-cased; empty means "not filtering".
 *
 *  One spelling on purpose. The rule that decides which groups are REVEALED
 *  ({@link revealedGroupKeys}, which drives what the page fetches) and the rule that decides what is
 *  SHOWN ({@link flattenTree}) have to agree — a trailing space normalized in one and not the other
 *  is a group that renders as matched and never loads its members. */
export function filterTerm(filter: string): string {
  return filter.trim().toLowerCase();
}

/** Concatenate node lists, keeping the FIRST entry for a repeated id.
 *
 *  Filter mode renders the server search page PLUS the members of the groups the term revealed, and
 *  a node is routinely in both lists. A duplicate is not a harmless extra row: `flatRowKey` is
 *  `n:<id>`, so it collides in the React key and in the virtualizer's `getItemKey`. */
export function mergeNodesById(...lists: NodeSummary[][]): NodeSummary[] {
  const seen = new Set<string>();
  const out: NodeSummary[] = [];
  for (const list of lists) {
    for (const n of list) {
      if (seen.has(n.id)) continue;
      seen.add(n.id);
      out.push(n);
    }
  }
  return out;
}

/** The group keys an active filter REVEALS: every group whose own name matches the term, plus that
 *  group's whole subtree.
 *
 *  A group matched by name shows its members even though none of them matched — the operator
 *  searched for the folder, so the folder's contents are the answer. Those members have to be
 *  fetched per group: filter mode's server search (`/nodes?search=`) matches a node's name/address
 *  and knows nothing about groups, so a matched folder's members are simply not in its answer.
 *
 *  Never includes {@link UNGROUPED} — nothing can match the bucket's name. Deterministic (the API
 *  returns groups ordered by sort_order, name, id) and CAPPED: a one-letter term matches most of a
 *  fleet's folders, and each key is one `/nodes/by-group` request, so this is the one place a
 *  keystroke can fan out into N requests — the fan-out the lazy tree exists to avoid. Past the cap a
 *  group degrades to the old behaviour (its matching nodes still show, its other members do not),
 *  never to a loading placeholder that nothing will ever resolve. */
export function revealedGroupKeys(groups: NodeGroup[], filter: string, cap: number): string[] {
  const q = filterTerm(filter);
  if (!q) return [];
  // 🚨 **Indexed once, not once per match** (ADR-133). This used to call `subtreeGroupIds(groups, …)`
  // per matching folder, and that helper rebuilds the whole parent→children index on entry — so a
  // one-letter term on a thousand folders was a thousand index builds, ~10^6 operations for an
  // answer the cap trims to 200. The cap bounded the RESULT, never the work.
  const childrenOf = childrenByParent(groups);
  const out: string[] = [];
  const seen = new Set<string>();
  for (const g of groups) {
    if (!g.name.toLowerCase().includes(q)) continue;
    for (const id of subtreeIdsFrom(childrenOf, g.id)) {
      if (seen.has(id)) continue;
      seen.add(id);
      out.push(id);
      if (out.length >= cap) return out;
    }
  }
  return out;
}

/** Whether a group's subtree contains anything matching `q` (its own name, a descendant group's
 *  name, or a member node's name) — so ancestor groups stay visible to reveal a nested match. */
function subtreeMatches(group: TreeGroup, q: string): boolean {
  if (group.name.toLowerCase().includes(q)) return true;
  if (group.nodes.some((n) => n.name.toLowerCase().includes(q))) return true;
  return group.children.some((c) => subtreeMatches(c, q));
}

/** Whether anything at all is under this group.
 *
 *  The counterpart of {@link subtreeMatches} for the `narrowed` mode, where there is no term to
 *  match against: the caller has already handed in exactly the nodes that survived a state / kind /
 *  pool filter, so "has a row" *is* "matches". */
function subtreeHasNodes(group: TreeGroup): boolean {
  return group.nodes.length > 0 || group.children.some(subtreeHasNodes);
}

/** Flatten the visible rows of the inventory tree in display order, honouring collapse state and
 *  the name filter — the single source of truth the virtualized `NodeTree` renders. Collapsed
 *  groups omit their descendants; while filtering, every group is force-expanded and non-matching
 *  rows are hidden. Pure (no React) so the ordering/visibility rules are unit-tested directly.
 *
 *  Lazy load (A-3): when `groupCounts` (server per-group direct counts) is supplied, group rows roll
 *  up from those — correct over the whole fleet even before members are fetched. `loadedGroups` says
 *  which groups' members have been fetched; an open group that isn't loaded yet emits a single
 *  `group-loading` placeholder instead of its members. Omit both for the legacy full-node path
 *  (rollup from loaded descendants, every group treated as loaded).
 *
 *  There are therefore **three** states for the counts, not two (ADR-133): supplied, absent because
 *  this caller has none, and `countsPending` — asked for and not yet answered, which is the state a
 *  progressive first paint spends its first round trip in. A pending row reports `tally: null` and
 *  still emits its `group-loading` placeholder, so the members start arriving while the counts are
 *  in flight rather than after them.
 *
 *  `revealedGroups` ({@link revealedGroupKeys}) is filter mode's counterpart: the groups whose whole
 *  membership is being fetched because the term matched the group's own name. Only those can be
 *  "still loading" while filtering — every other group is showing the search page's hits and nothing
 *  more, so a group past the reveal cap gets no members rather than a placeholder that never
 *  resolves. */
export function flattenTree(
  tree: NodeTreeData,
  opts: {
    collapsed: Record<string, boolean>;
    filter: string;
    /** The nodes handed in have already been narrowed by a filter this function cannot see — the
     *  tree's state / kind / pool controls, which are applied server-side (ADR-053 Inc.6).
     *
     *  🚨 **Without this the tree cannot tell it is filtering at all.** Every "are we filtering"
     *  test below used to be `the term is non-empty`, so picking *Critical* with an empty search
     *  box hid nothing: every group stayed on screen, including the ones with no critical node
     *  under them, and collapsed groups stayed collapsed over their own matches. */
    narrowed?: boolean;
    groupCounts?: Record<string, StateCounts>;
    /** The per-group counts have been ASKED FOR and have not arrived (ADR-133).
     *
     *  🚨 **This is what makes the first member fetch happen at all.** The fetch set is derived
     *  from the `group-loading` rows this function emits ({@link pendingGroupKeys}), a placeholder
     *  is emitted when `directTotal > shown.length`, and `directTotal` comes from `groupCounts` —
     *  so with no counts yet that test is `0 > 0` and **not one folder asks for its members**.
     *  The tree would sit on a complete skeleton, fetching nothing, until the counts landed.
     *
     *  ⚠️ **It is not only a first-paint state.** `NodesPage` used to hand `{}` in place of a
     *  FAILED `/fleet/group-summary`, which reads identically — so a deployment whose summary
     *  endpoint was erroring showed every folder as empty and never loaded a single member, with
     *  nothing on screen saying why. Distinguishing "not answered" from "answered: zero" is the
     *  whole point of the flag.
     *
     *  Omit it on the legacy full-node path: there the counts are not late, they do not exist. */
    countsPending?: boolean;
    loadedGroups?: Set<string>;
    revealedGroups?: Set<string>;
    /** Groups whose member fetch failed (ADR-125). They emit a `group-failed` row instead of the
     *  `group-loading` placeholder — otherwise a folder nobody can load is drawn exactly like one
     *  that is still arriving, forever. */
    failedGroups?: Set<string>;
  },
): FlatRow[] {
  const q = filterTerm(opts.filter);
  // Two different questions, and conflating them was the bug. `byTerm` decides what *matches a
  // name*; `narrowing` decides whether the tree is showing a filtered set at all — which is what
  // force-expansion, hiding an empty folder and the ungrouped header turn on.
  const byTerm = q.length > 0;
  const narrowing = byTerm || opts.narrowed === true;
  const rows: FlatRow[] = [];
  const counts = opts.groupCounts;
  // The counts are on their way (ADR-133). Everything below asks this BEFORE it asks `counts`,
  // because the caller may legitimately hand an empty object in the meantime and an empty object
  // is indistinguishable from "every folder is empty" once you are reading values out of it.
  const pending = opts.countsPending === true;
  // Per-group subtree tally from the server direct counts (bottom-up over the built, acyclic tree).
  // Skipped while pending: it would walk every group to produce zeros that nothing reads.
  const subtree = counts && !pending ? subtreeTallyMap(tree.roots, counts) : null;
  // Browsing: a group whose members haven't been fetched stands in with a placeholder. Filtering:
  // the server search page carries every match there is, so only a REVEALED group (whose members are
  // being fetched separately, because the term matched the folder rather than its contents) can be
  // waiting on anything.
  const isLoaded = (id: string) =>
    narrowing
      ? !opts.revealedGroups?.has(id) || (opts.loadedGroups?.has(id) ?? true)
      : !opts.loadedGroups || opts.loadedGroups.has(id);

  /**
   * The nodes under `group` that this pass will actually render — the group row's tally while
   * narrowing.
   *
   * ⚠️ **It has to mirror `walkGroup`'s own rules, including the inherited match.** A folder whose
   * *name* matches the term shows all of its members, so its count must too; deriving the number
   * from anything simpler would put a figure beside the bar that the rows below contradict.
   */
  const visibleNodes = (group: TreeGroup, ancestorMatch: boolean): NodeSummary[] => {
    const eff = ancestorMatch || (byTerm && group.name.toLowerCase().includes(q));
    const own = group.nodes.filter((n) => !byTerm || eff || n.name.toLowerCase().includes(q));
    return [...own, ...group.children.flatMap((c) => visibleNodes(c, eff))];
  };

  const walkGroup = (group: TreeGroup, depth: number, ancestorMatch: boolean): void => {
    // A folder's own name can only be matched by a term. A state filter says nothing about it.
    const selfMatch = byTerm && group.name.toLowerCase().includes(q);
    const effMatch = ancestorMatch || selfMatch;
    // Hide a group entirely when nothing under it survives the narrowing. With a term that means
    // "no name below matches"; without one it means "no rows below at all", because the caller
    // already removed the rows that did not survive.
    // 🚨 **Only `narrowing` needs this, so only `narrowing` may pay for it** (ADR-125). Both
    // branches walk the group's whole subtree, and this used to be computed into a `const` above
    // the `if` — so every browse-mode flatten walked every subtree and threw the answer away one
    // line later. That is O((groups + nodes) × depth) per flatten, and a flatten runs once per
    // arriving `/nodes/by-group` response. `&&` short-circuits, so browsing now walks nothing.
    if (narrowing && !effMatch && !(byTerm ? subtreeMatches(group, q) : subtreeHasNodes(group))) {
      return;
    }

    const isOpen = narrowing ? true : !opts.collapsed[group.id];
    // 🚨 **While narrowing, the bar describes the rows on screen — not the fleet.** The server
    // rollup (`groupCounts`) is the group's whole membership and is the right answer when browsing,
    // where the row stands in for a folder nobody has opened. Under a filter it is a different
    // statement from the one the operator is reading: "DNS 3" beside a single row asks them to
    // work out which number is the answer. Chosen deliberately (2026-08-14) over keeping the
    // health rollup; the cost is that "how big is this folder really" is not visible while a
    // filter is on.
    // ⚠️ `pending` is asked before `subtree`, not after. Narrowing still wins outright: it tallies
    // the rows it is about to draw, which needs no server answer.
    const tally: StateTally | null = narrowing
      ? tallyStates(visibleNodes(group, ancestorMatch))
      : pending
        ? null
        : subtree
          ? subtree.get(group.id) ?? tallyFromCounts(emptyStateCounts())
          : tallyStates(descendantNodes(group));
    const directTotal = counts ? countsTotal(counts[group.id] ?? emptyStateCounts()) : group.nodes.length;
    // A twisty is offered when the group has sub-groups or any (counted or loaded) member below it
    // — and, while the counts are pending, whenever the members have not arrived either. We cannot
    // yet know the folder is empty, and refusing to open a folder that has members is the worse of
    // the two mistakes; the twisty disappears on its own for the folders the counts report as empty.
    // ⚠️ `group.nodes.length` earns its place only in the pending case, where `tally` is null and
    // `directTotal` is 0 even for a folder whose members have already arrived. Without it a folder
    // that loaded its members while the counts never did would offer a DISABLED twisty — open, with
    // no way to close it.
    const hasChildren =
      group.children.length > 0 ||
      (tally?.total ?? 0) > 0 ||
      group.nodes.length > 0 ||
      (pending && !isLoaded(group.id));
    rows.push({ kind: 'group', depth, group, isOpen, hasChildren, tally });
    if (!isOpen) return;
    // Children first, then this group's own member nodes — matching the recursive render order.
    for (const child of group.children) walkGroup(child, depth + 1, effMatch);
    // Only a term rejects a node here — the state / kind / pool filters already did their rejecting
    // server-side, so every node still in hand is one the operator asked for.
    const shown = group.nodes.filter(
      (n) => !byTerm || effMatch || n.name.toLowerCase().includes(q),
    );
    for (const n of shown) rows.push({ kind: 'node', depth: depth + 1, node: n });
    // Members still arriving: one placeholder standing in for the rest. What we already have goes
    // first — in filter mode the search page already carries this group's MATCHING nodes, and hiding
    // them behind the placeholder would flicker them out while the rest of the folder loads.
    // 🚨 **`pending ||` is what starts the very first fetch.** `directTotal` is 0 until the counts
    // land, so without it this reads `0 > 0` for every folder and the tree asks for nothing — see
    // `countsPending`'s own note, which is where the failure that motivated it is written down.
    if (!isLoaded(group.id) && (pending || directTotal > shown.length)) {
      // A failed fetch is not a slow one, and drawing it as one leaves the operator waiting on
      // something that is never coming (ADR-125). The failed row carries the retry control.
      const kind = opts.failedGroups?.has(group.id) ? 'group-failed' : 'group-loading';
      rows.push({ kind, depth: depth + 1, groupId: group.id });
    }
  };

  for (const g of tree.roots) walkGroup(g, 0, false);

  const ungroupedShown = byTerm
    ? tree.ungrouped.filter((n) => n.name.toLowerCase().includes(q))
    : tree.ungrouped;
  // Show the ungrouped header + its root drop zone whenever there's any inventory (so the drop zone
  // is reachable next to the groups), but not while narrowing with nothing ungrouped to show, and
  // not for a completely empty inventory (the page shows its own empty-state message instead).
  const showUngrouped = narrowing
    ? ungroupedShown.length > 0
    : tree.roots.length > 0 || tree.ungrouped.length > 0;
  if (showUngrouped) {
    rows.push({ kind: 'ungrouped-head', count: tree.ungrouped.length });
    for (const n of ungroupedShown) rows.push({ kind: 'ungrouped-node', depth: 1, node: n });
  }
  return rows;
}

/** A per-state tally of a node set, plus how many of them need attention. Counts cover every
 *  `NodeState`, so a missing state is simply `0` (handy for proportional bar widths). */
export interface StateTally {
  counts: Record<NodeState, number>;
  total: number;
  needAttention: number;
}

/** Count a node set by state. Drives the health bar segment widths, the per-state legend, and the
 *  "N need attention" summary on group rollups and the page header. */
export function tallyStates(nodes: NodeSummary[]): StateTally {
  const counts = emptyStateCounts();
  let needAttention = 0;
  for (const n of nodes) {
    counts[n.state] += 1;
    if (PROBLEM_STATES.has(n.state)) needAttention += 1;
  }
  return { counts, total: nodes.length, needAttention };
}

/** A raw per-state count object — the `/fleet/group-summary` value shape (server per-group rollup,
 *  A-1/A-3). Structurally identical to `StateTally.counts`. */
export type StateCounts = Record<NodeState, number>;

/** Sum of every state count in a per-state tally. */
export function countsTotal(c: StateCounts): number {
  return DISPLAY_ORDER.reduce((n, s) => n + (c[s] ?? 0), 0);
}

/** Build a `StateTally` (counts + total + need-attention) from a raw per-state count object — the
 *  counts-driven twin of {@link tallyStates}, for the server-side per-group rollup (A-1/A-3). */
export function tallyFromCounts(counts: StateCounts): StateTally {
  let total = 0;
  let needAttention = 0;
  for (const s of DISPLAY_ORDER) {
    const n = counts[s] ?? 0;
    total += n;
    if (PROBLEM_STATES.has(s)) needAttention += n;
  }
  return { counts, total, needAttention };
}

/** Roll each group's DIRECT member counts up its subtree, yielding a per-group DESCENDANT tally
 *  (the whole subtree's health) from the server per-group direct counts (A-3). Bottom-up over the
 *  built tree, which is acyclic (each group appears once), so no cycle guard is needed.
 *
 *  ⚠️ **Exported so the group detail pane can roll up the same way the tree row does** (ADR-125).
 *  It used to derive its own tally from the members that happened to be LOADED, while the row beside
 *  it used these server counts — so the two could show different numbers for the same folder. Same
 *  question, one answer (`extensibility.md` §3). */
export function subtreeTallyMap(
  roots: TreeGroup[],
  counts: Record<string, StateCounts>,
): Map<string, StateTally> {
  const out = new Map<string, StateTally>();
  const visit = (g: TreeGroup): StateCounts => {
    const acc = emptyStateCounts();
    const own = counts[g.id];
    if (own) for (const s of DISPLAY_ORDER) acc[s] += own[s] ?? 0;
    for (const child of g.children) {
      const sub = visit(child);
      for (const s of DISPLAY_ORDER) acc[s] += sub[s];
    }
    out.set(g.id, tallyFromCounts(acc));
    return acc;
  };
  for (const r of roots) visit(r);
  return out;
}

/** Read a group's `group_type` off the wire, where it is a bare string (the server validates it at
 *  the write edge, so the closed set only exists in TypeScript — see `GROUP_TYPES`). Anything
 *  unrecognised reads as `generic`, which is the plain-folder rendering the icon already fell back
 *  to. The single narrowing site, so the picker, the tree and the detail pane cannot disagree. */
export function asGroupType(value: string | undefined): GroupType {
  return GROUP_TYPES.find((g) => g === value) ?? 'generic';
}

/** The chain of group names from the top-level ancestor down to `groupId` (inclusive), for the
 *  detail-pane breadcrumb eyebrow (e.g. `Tokyo / Edge / Firewall`). Empty for a null/unknown id.
 *  Bounded by the group count so malformed (cyclic) data can't loop forever. */
export function groupPath(groups: NodeGroup[], groupId: string | null): string[] {
  if (!groupId) return [];
  const byId = new Map(groups.map((g) => [g.id, g]));
  const out: string[] = [];
  let cur = byId.get(groupId);
  for (let i = 0; cur && i <= groups.length; i++) {
    out.unshift(cur.name);
    cur = cur.parent_id ? byId.get(cur.parent_id) : undefined;
  }
  return out;
}

/** One folder as a picker offers it.
 *
 *  ⚠️ **`label` is the folder's own name, with no indent baked in** (ADR-124 決定 9). It used to
 *  carry two full-width spaces per level, which made the depth un-styleable, put invisible
 *  characters into every search term the operator's text was compared against, and — the half
 *  that actually misleads — kept drawing an indent after filtering had removed the parent the
 *  indent was relative to. Depth is data now; the picker draws it, and shows `path` instead while
 *  a search term is narrowing the list. */
export interface GroupOption {
  id: string;
  /** The folder's own name. */
  label: string;
  /** How many folders sit above it, 0 at the root. */
  depth: number;
  /** Every name from the root down, joined — `Tokyo / Edge / FW`. */
  path: string;
}

/** Flatten the group hierarchy into depth-ordered options, so the tree shape reads in a flat list
 *  (used by every folder picker). Siblings are sorted by name; the walk is depth-first, so a
 *  folder is immediately followed by its subtree. */
export function groupOptions(groups: NodeGroup[]): GroupOption[] {
  const byParent = new Map<string | null, NodeGroup[]>();
  for (const g of groups) {
    pushInto(byParent, g.parent_id ?? null, g);
  }
  const out: GroupOption[] = [];
  const walk = (parent: string | null, depth: number, trail: string[]) => {
    for (const g of (byParent.get(parent) ?? []).sort((a, b) => a.name.localeCompare(b.name))) {
      const path = [...trail, g.name];
      out.push({ id: g.id, label: g.name, depth, path: path.join(' / ') });
      walk(g.id, depth + 1, path);
    }
  };
  walk(null, 0, []);
  return out;
}

/** Narrow folder options by a typed term, case-insensitively.
 *
 *  Matches the **whole path**, so typing a site name keeps the racks under it — which is what an
 *  operator means by "show me Tokyo". An empty or blank term is not a filter and returns the list
 *  untouched, rather than the empty list a naive `includes('')` walk would suggest.
 *
 *  Client-side on purpose: folders are bounded by what an operator (or a NetBox sync) created, not
 *  by fleet size, and the list is already in the browser — `ui-conventions.md` allows exactly this
 *  case, and a server round trip per keystroke would be slower than the filter it replaced. */
export function filterGroupOptions(
  options: readonly GroupOption[],
  term: string,
): GroupOption[] {
  const q = term.trim().toLowerCase();
  if (!q) return [...options];
  return options.filter((o) => o.path.toLowerCase().includes(q));
}

/** Sentinel key for the ungrouped bucket in the per-group member cache and the `/nodes/by-group`
 *  call (which takes `null` for it). A sentinel rather than `null` so the cache can stay a plain
 *  `Record<string, …>` keyed the same way for both. */
export const UNGROUPED = '__ungrouped__';

/** The groups in these rows that are waiting for their members — i.e. the fetch set (ADR-125).
 *
 *  🚨 **Derived from the `group-loading` rows, never from the `group` rows.** A loading row is
 *  emitted for exactly the folders that are open, unloaded and non-empty (see `flattenTree`'s
 *  `isLoaded` and the `directTotal > shown.length` test), so the set that gets FETCHED and the set
 *  the tree draws a placeholder for cannot disagree — the same guarantee `revealedGroups` already
 *  carries. Collapsed folders, empty ones and already-loaded ones drop out for free, with no second
 *  copy of those three rules.
 *
 *  ⚠️ **`group-failed` rows are deliberately excluded.** A failed folder is retried only when the
 *  operator asks (ADR-125 decision 2); including it here would put the automatic retry back, one
 *  level up, where the queue could not see it either.
 *
 *  Hand it the rows the virtualizer is actually showing and the fetch follows the viewport; hand it
 *  every row and it degrades to "everything open", which is what this replaced. */
export function pendingGroupKeys(rows: readonly FlatRow[]): string[] {
  const out: string[] = [];
  for (const r of rows) if (r.kind === 'group-loading') out.push(r.groupId);
  return out;
}

// A `stableKeys(prev, next)` helper lived here briefly and is gone. It existed to keep the fetch
// set's array IDENTITY stable across scroll frames, so an effect keyed on it would not re-run. The
// caller needed a settle anyway (a momentum scroll must not queue every folder it passes, only the
// ones it lands on), and debouncing the keys as a joined STRING compares content for free — so the
// identity helper became a second answer to a question already settled one line above it.

// `visibleOpenGroupKeys(groups, collapsed)` lived here and is gone (ADR-125). It answered "every
// folder with no collapsed ancestor", which the member cache used as its fetch set — and since
// collapse state defaults to empty, that was EVERY folder, so a 500-folder deployment fired 501
// requests on first paint. Its name said "visible" and its doc said "actually on screen"; neither
// was true, and the gap was invisible from the screen it produced, because a folder row is drawn
// from the server rollup whether or not its members are loaded.
//
// What replaced it is {@link pendingGroupKeys}, which reads the rows the virtualizer is showing —
// so "on screen" is answered by the thing that decides what is on screen, rather than by a second
// implementation of it. Deleted rather than kept for a future caller: there was none, and a
// plausible-looking function that answers a *slightly different* question is exactly how this one
// came to be used for the wrong thing.

/** A group id plus every descendant group id (its whole subtree). Used to lazily load a selected
 *  group's subtree so the detail pane can roll up its members. Cycle-guarded by the visited set —
 *  this walks the raw `parent_id` edges from the API, not the built (acyclic) tree. */
export function subtreeGroupIds(groups: NodeGroup[], rootId: string): string[] {
  return subtreeIdsFrom(childrenByParent(groups), rootId);
}

/** Index the folder list by parent id — the shape every subtree walk below needs.
 *
 *  Split out so a caller that walks MANY subtrees builds it once ({@link revealedGroupKeys}).
 *  Building it per walk is O(groups) each time, which is invisible at forty folders and is the
 *  difference between O(G) and O(G²) at a thousand. */
function childrenByParent(groups: NodeGroup[]): Map<string, NodeGroup[]> {
  const out = new Map<string, NodeGroup[]>();
  for (const g of groups) {
    if (g.parent_id) pushInto(out, g.parent_id, g);
  }
  return out;
}

/** {@link subtreeGroupIds} over an index the caller already holds. Cycle-guarded by the visited
 *  set — this walks the raw `parent_id` edges from the API, not the built (acyclic) tree. */
function subtreeIdsFrom(childrenOf: Map<string, NodeGroup[]>, rootId: string): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  const walk = (id: string) => {
    if (seen.has(id)) return;
    seen.add(id);
    out.push(id);
    for (const c of childrenOf.get(id) ?? []) walk(c.id);
  };
  walk(rootId);
  return out;
}

/** Whether `candidateId` is `ancestorId` itself or sits anywhere below it in the group tree.
 *  Used to forbid moving a group into its own subtree (which would create a cycle). Bounded by
 *  the group count so malformed (already-cyclic) data can't loop forever. */
export function isSelfOrDescendant(
  groups: NodeGroup[],
  ancestorId: string,
  candidateId: string,
): boolean {
  if (ancestorId === candidateId) return true;
  const parentOf = new Map(groups.map((g) => [g.id, g.parent_id]));
  let cur: string | null | undefined = candidateId;
  for (let i = 0; i <= groups.length; i++) {
    if (cur == null) return false;
    if (cur === ancestorId) return true;
    cur = parentOf.get(cur) ?? null;
  }
  return true;
}

// `filterResultsTruncated(filtering, count, cap)` lived here and is gone. It inferred "matches are
// missing" from a full page, which held while the only filter was a text search the SQL served
// whole. It stopped holding when the tree gained state / kind / pool: the server rejects those
// candidates *after* the query, so a short page and a complete answer are no longer the same
// thing — a scan of 5,000 rows that keeps 3 returns three rows and is still incomplete, and the
// inference would have called that complete precisely when it was least so.
//
// `NodePage.truncated` is the server's own answer now. Deleted rather than left for the text-only
// case: a helper that is right for one caller and quietly wrong for the next is worse than none.

/** Find a group's built node (with children + nodes resolved) anywhere in the tree. */
export function findTreeGroup(roots: TreeGroup[], id: string): TreeGroup | null {
  for (const g of roots) {
    if (g.id === id) return g;
    const hit = findTreeGroup(g.children, id);
    if (hit) return hit;
  }
  return null;
}

/** One-line impact summary for deleting a group: how many direct subgroups and member nodes
 *  will be re-parented (nothing is deleted). The member count comes from the server per-group
 *  rollup (A-3) so it's correct without loading the group's members. Pluralised for readability. */
export function groupDeletionImpact(
  groups: NodeGroup[],
  groupCounts: Record<string, StateCounts>,
  g: NodeGroup,
  t: TFunction,
): string {
  const subs = groups.filter((x) => x.parent_id === g.id).length;
  const members = groupCounts[g.id] ? countsTotal(groupCounts[g.id]) : 0;
  return t('deleteGroup.impact', {
    subgroups: t('count.subgroup', { count: subs }),
    members: t('count.memberNode', { count: members }),
  });
}
