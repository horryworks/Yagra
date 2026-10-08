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
