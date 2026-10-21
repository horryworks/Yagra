// SPDX-License-Identifier: AGPL-3.0-only
// Bringing the selection back into view when the operator stops narrowing the tree (ADR-073 Inc.2).
//
// The failure this answers: narrow the tree, pick a node, clear the filter — and the node is gone
// from the tree while the right pane still shows it. `?sel=` never changed. The ROW did not exist:
// browsing loads a folder's members only once the folder is on screen, the saved layout closes the
// folders the search had opened, and nothing scrolled. A selection with no row reads as a selection
// that was lost.
//
// So the page asks for a reveal once, at the moment the last narrowing control goes, and the tree
// opens the folders above the selection, waits for the row to exist, and scrolls to it. It follows
// the row only while folders above it are still filling in (`revealHolds`), and lets go the moment
// the operator wheels, presses or types in the tree — scrolling the tree for the operator is what
// ADR-124 Inc.5 refused, and this is the exception only because the operator's own press is what
// hid the row.
//
// ⚠️ Every decision is here, in a `.ts`, because Vitest never loads a `.tsx` (testing.md).

import { groupTrail, UNGROUPED, type FlatRow } from '../../lib/nodeTree';
import type { NodeGroup } from '../../types/api';
import { indexOfSelection } from './nodeTreeKeys';
// Type-only, so nothing here loads a `.tsx` — the shape `nodeTreeKeys.ts` uses.
import type { TreeSelection } from './NodeTree';

/** One request to bring a selection into view. `seq` tells two requests for the same row apart. */
export interface RevealRequest {
  sel: NonNullable<TreeSelection>;
  /** The folder the selected node lives in (`null` = Ungrouped). For a selected folder, its parent. */
  groupId: string | null;
  seq: number;
  /** The operator pressed something that names this row (a folder in the detail pane's breadcrumb),
   *  so it is shown even while the tree is narrowed — if the narrowed tree draws it. */
  asked?: true;
  /** The tree put back where it was left — a reload, or coming back from another screen
   *  (`anchorAt`): the row's top edge sat this many pixels below the top of the pane. Present ⇒
   *  the row goes back to that spot, not to the centre, and no folder is opened — the layout it
   *  was taken from already had them open. */
  top?: number;
}

/**
 * The row at the top of the pane, as a request that puts it back there (ADR-073 Inc.3).
 *
 * 🚨 **A row, not a pixel offset.** Members load only once their folder is on screen, so after a
 * reload every folder above the old position is one placeholder row again until it answers: the
 * same `scrollTop` lands hundreds of rows further down the tree. The row is what the operator saw.
 *
 * Walks down from the first row on screen to the first folder or node, because a loading
 * placeholder or the Ungrouped header names nothing a reload can find again. Null at the very top
 * (nothing to restore) and when no such row is drawn.
 */
export function anchorAt(
  drawn: readonly FlatRow[],
  scrollTop: number,
  rowHeight: number,
): RevealRequest | null {
  if (scrollTop <= 0 || rowHeight <= 0) return null;
  for (let i = Math.floor(scrollTop / rowHeight); i < drawn.length; i++) {
    const row = drawn[i];
    const top = i * rowHeight - scrollTop;
    if (row.kind === 'group') {
      const sel = { kind: 'group', id: row.group.id } as const;
      return { sel, groupId: row.group.parent_id ?? null, seq: 0, top };
    }
    if (row.kind === 'node' || row.kind === 'ungrouped-node') {
      const sel = { kind: 'node', id: row.node.id } as const;
      return { sel, groupId: row.node.group_id ?? null, seq: 0, top };
    }
  }
  return null;
}

/** What to store when the tree goes away: the row at the top of the pane, or `undefined` — keep
 *  the point already stored — while a restore is still pending. Until it has scrolled and been let
 *  go, the pane is not where the operator left it, and reading it would forget the real point. */
export function anchorToSave(
  restorePending: boolean,
  drawn: readonly FlatRow[],
  scrollTop: number,
  rowHeight: number,
): RevealRequest | null | undefined {
  return restorePending ? undefined : anchorAt(drawn, scrollTop, rowHeight);
}

/** Where the restore step scrolls to: the row's top `top` pixels below the pane's top. */
export function restoreOffset(index: number, rowHeight: number, top: number): number {
  return Math.max(0, index * rowHeight - top);
}

/**
 * What a reload's restore does against the rows drawn now: scroll once the row is drawn, and wait
 * until then — the folder list, its members and the search page all arrive after the first render.
 *
 * ⚠️ Never `done` by itself. A row that never comes (deleted, filtered out) leaves the request
 * waiting, and the tree ends it on a timer or on the operator's first wheel, press or key.
 */
export function restoreStep(drawn: readonly FlatRow[], req: RevealRequest): RevealStep {
  const index = indexOfSelection(drawn, req.sel);
  return index >= 0 ? { kind: 'scroll', index } : { kind: 'wait' };
}

/** What a narrowing control coming into force does to the reveal in hand. Starting to narrow ends
 *  it — but not a reload's restore when the tree loaded already narrowed, or when the narrowing is
 *  Pinned only taking effect because this load's pins just arrived: neither is the operator
 *  starting anything, and the saved point was taken from that narrowed tree. */
export function revealOnNarrowing(
  r: RevealRequest | null,
  wasNarrowed: boolean,
  pinsArrived: boolean,
): RevealRequest | null {
  return (wasNarrowed || pinsArrived) && r?.top !== undefined ? r : null;
}

/** Whether a reveal waits out the tree's filter. One the operator asked for does not: if the
 *  narrowed tree draws the row it is shown now, and if not there is nothing to wait for. */
export function waitsOnFilter(filtering: boolean, req: RevealRequest): boolean {
  return filtering && !req.asked;
}

/** The history state a link sends with `?sel=` when the tree should show that row on arrival — the
 *  standalone node page's breadcrumb, which has no tree of its own to show it in. */
export const REVEAL_ARRIVAL = { revealSelection: true } as const;

/** Whether a location's state is that mark. History state is anything any code wrote, so this
 *  reads it rather than trusting a type. */
export function isRevealArrival(state: unknown): boolean {
  return (
    typeof state === 'object' &&
    state !== null &&
    (state as { revealSelection?: unknown }).revealSelection === true
  );
}

/** Where the tree keeps the restore point across a reload or a visit to another screen. One key:
 *  there is one inventory tree. */
export const TREE_ANCHOR_KEY = 'yagra.nodeTree.anchor';

/** This tab's storage, or undefined where there is none (Vitest's node environment) or where even
 *  reaching for it throws (blocked site data). */
export function sessionStorageOrUndefined(): Storage | undefined {
  try {
    return typeof sessionStorage === 'undefined' ? undefined : sessionStorage;
  } catch {
    return undefined;
  }
}

/** Persist (or, given null, forget) the restore point. Storage may be missing or refuse a write
 *  (private windows, blocked site data); losing the position is then the whole cost. */
export function writeTreeAnchor(store: Storage | undefined, anchor: RevealRequest | null): void {
  try {
    if (!store) return;
    if (!anchor || anchor.top === undefined) {
      store.removeItem(TREE_ANCHOR_KEY);
      return;
    }
    const { sel, groupId, top } = anchor;
    store.setItem(TREE_ANCHOR_KEY, JSON.stringify({ sel, groupId, top }));
  } catch {
    // Nothing to do: the next reload starts at the top, as it did before this existed.
  }
}

/** The restore point a previous load of this tab left, or null. Anything malformed is ignored —
 *  the value comes back from storage, not from this build. `seq` is -1, which no page-made
 *  request uses (they count up from 1). */
export function readTreeAnchor(store: Storage | undefined): RevealRequest | null {
  try {
    const raw = store?.getItem(TREE_ANCHOR_KEY);
    if (!raw) return null;
    const v = JSON.parse(raw) as {
      sel?: { kind?: unknown; id?: unknown };
      groupId?: unknown;
      top?: unknown;
    };
    const kind = v.sel?.kind;
    const id = v.sel?.id;
    if ((kind !== 'node' && kind !== 'group') || typeof id !== 'string') return null;
    if (typeof v.top !== 'number' || !Number.isFinite(v.top)) return null;
    if (v.groupId !== null && typeof v.groupId !== 'string') return null;
    return { sel: { kind, id }, groupId: v.groupId, seq: -1, top: v.top };
  } catch {
    return null;
  }
}

/**
 * The request to make when narrowing ends, or null when there is nothing to reveal.
 *
 * A node's folder is only known if the page saw the node's row while narrowing (`homeGroupId`,
 * `undefined` when it never did — a pasted `?sel=`, say). Guessing would scroll to the wrong
 * folder; doing nothing leaves the tree as it was before this existed.
 */
export function revealRequestFor(
  sel: TreeSelection,
  groups: readonly NodeGroup[],
  homeGroupId: string | null | undefined,
  seq: number,
): RevealRequest | null {
  if (!sel) return null;
  if (sel.kind === 'group') {
    const group = groups.find((g) => g.id === sel.id);
    if (!group) return null;
    return { sel, groupId: group.parent_id ?? null, seq };
  }
  if (homeGroupId === undefined) return null;
  return { sel, groupId: homeGroupId, seq };
}

/** The folders that must be open for the selection's row to be drawn: the whole chain from the
 *  root down to `groupId`. Empty for Ungrouped and for a top-level folder. */
export function foldersToOpen(groups: NodeGroup[], groupId: string | null): string[] {
  return groupTrail(groups, groupId).map((c) => c.id);
}

/** `collapsed` with every one of `ids` taken out — the same object when none was in it, so a
 *  reveal over folders that are already open writes nothing to the account. */
export function openFolders(
  collapsed: Readonly<Record<string, true>>,
  ids: readonly string[],
): Readonly<Record<string, true>> {
  if (!ids.some((id) => collapsed[id])) return collapsed;
  const next = { ...collapsed };
  for (const id of ids) delete next[id];
  return next;
}

export type RevealStep =
  /** Not yet: the tree is still narrowed, a folder is still closed, or the members are on the way. */
  | { kind: 'wait' }
  /** Scroll to this row of `drawn`, and the request is done. */
  | { kind: 'scroll'; index: number }
  /** The row will not come (the folder answered without it); the request is done, nothing moves. */
  | { kind: 'done' };

/**
 * What a reveal does against the rows drawn now.
 *
 * 🚨 **`filtering` is the TREE's own, not the page's.** The tree narrows by the applied term, which
 * trails the box by the search debounce; for that moment the node's search row is still drawn, and
 * scrolling to it would land on a row that is about to move.
 *
 * 🚨 **Closed folders are waited out before "loaded but absent" is believed.** Opening them is a
 * store write that reaches `drawn` a render later; a folder already loaded but still closed would
 * otherwise read as "the node is not in it" and give up on the first frame.
 */
export function revealStep(
  drawn: readonly FlatRow[],
  req: RevealRequest,
  ctx: {
    filtering: boolean;
    collapsed: Readonly<Record<string, true>>;
    folders: readonly string[];
    /** Folders whose members are in; undefined on the legacy path where every folder is. */
    loadedGroups: ReadonlySet<string> | undefined;
  },
): RevealStep {
  if (ctx.filtering) return { kind: 'wait' };
  if (ctx.folders.some((id) => ctx.collapsed[id])) return { kind: 'wait' };
  const index = indexOfSelection(drawn, req.sel);
  if (index >= 0) return { kind: 'scroll', index };
  // A folder row is always drawn once its parents are open; if it is not, it is gone.
  if (req.sel.kind === 'group') return { kind: 'done' };
  const key = req.groupId ?? UNGROUPED;
  // A failed fetch: show the folder's retry row, which is where the node would have been.
  const failed = drawn.findIndex((r) => r.kind === 'group-failed' && r.groupId === key);
  if (failed >= 0) return { kind: 'scroll', index: failed };
  if (ctx.loadedGroups && !ctx.loadedGroups.has(key)) return { kind: 'wait' };
  // Loaded without it — a capped folder, or the node moved meanwhile. The folder is the closest
  // thing to where it was; Ungrouped has no folder row of its own to go to.
  const folder =
    req.groupId === null
      ? -1
      : drawn.findIndex((r) => r.kind === 'group' && r.group.id === req.groupId);
  return folder >= 0 ? { kind: 'scroll', index: folder } : { kind: 'done' };
}

/**
 * Whether a reveal that has scrolled may let go, or must stay to scroll again (ADR-073 Inc.2, ④).
 *
 * 🚨 **One scroll was not enough, and the lab measured why.** The scroll lands on the row's index
 * at that moment, and nothing anchors it afterwards: a folder ABOVE the row whose members arrive
 * later turns its one placeholder row into a hundred, and the row slides a hundred rows down — off
 * the pane, ~3,000px short on a deployment of ~3,000 nodes. The target's own folder is a single
 * request the reveal asks for at once; the folders on screen before the scroll go out together
 * after the viewport settles, so a big target folder easily answers FIRST.
 *
 * So the reveal holds while any placeholder above the row can still turn into rows:
 *  - one whose members are queued or in flight (`loading`), and
 *  - one the virtualizer is drawing now (`onScreen`) — it is about to be asked for, once the
 *    viewport settles, and is not in `loading` yet.
 * A placeholder that is neither stays one row until the operator scrolls to it, and an operator
 * scrolling ends the reveal anyway. A failed folder draws `group-failed`, which never grows.
 *
 * ⚠️ Rows BELOW the target are not waited on: they cannot move it.
 */
export function revealHolds(
  drawn: readonly FlatRow[],
  index: number,
  loading: ReadonlySet<string>,
  onScreen: ReadonlySet<string>,
): boolean {
  for (let i = 0; i < index && i < drawn.length; i++) {
    const row = drawn[i];
    if (row.kind === 'group-loading' && (loading.has(row.groupId) || onScreen.has(row.groupId))) {
      return true;
    }
  }
  return false;
}

/** The longest a reveal follows its row, from its first scroll. Past it the reveal lets go whatever
 *  is still loading: a folder above that never answers (or is never asked for) would otherwise
 *  keep the tree re-scrolling on every later change above the row, with no input from the operator
 *  to end it (ADR-124 Inc.5). Long enough for the folders on screen to answer on a slow core. */
export const REVEAL_FOLLOW_MS = 10_000;

/** Whether a reveal that began following its row at `since` has followed it long enough. */
export function revealOutstayed(since: number, now: number): boolean {
  return now - since >= REVEAL_FOLLOW_MS;
}
