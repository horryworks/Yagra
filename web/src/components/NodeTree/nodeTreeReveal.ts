// SPDX-License-Identifier: AGPL-3.0-only
// Bringing the selection back into view when the operator stops narrowing the tree (ADR-073 増分 2).
//
// The failure this answers: narrow the tree, pick a node, clear the filter — and the node is gone
// from the tree while the right pane still shows it. `?sel=` never changed. The ROW did not exist:
// browsing loads a folder's members only once the folder is on screen, the saved layout closes the
// folders the search had opened, and nothing scrolled. A selection with no row reads as a selection
// that was lost.
//
// So the page asks for a reveal once, at the moment the last narrowing control goes, and the tree
// opens the folders above the selection, waits for the row to exist, and scrolls to it ONE time.
// It never follows the row afterwards — scrolling the tree for the operator is what ADR-124 増分 5
// refused, and this is the exception only because the operator's own press is what hid the row.
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
