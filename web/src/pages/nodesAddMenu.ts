// SPDX-License-Identifier: AGPL-3.0-only
// What the inventory pane's ＋ menu acts on.
//
// The ＋ used to be an "add group at top level" button; it is now a two-item menu (add node / add
// group) whose target follows the tree selection, the same way the right-click menu does. The
// derivation lives here rather than in NodesPage.tsx because Vitest only runs `.test.ts` — a
// judgement made inside a `.tsx` is a judgement nothing can test.

import type { TreeSelection } from '../components/NodeTree/NodeTree';
import type { NodeGroup, NodeSummary } from '../types/api';

/** The label keys the ＋ menu can use (`nodes` namespace). An `as const` array so the i18n
 *  key-coverage test can iterate it — nothing types `t()` against a key union. */
export const ADD_MENU_LABEL_KEYS = [
  'tree.addNodeEllipsis',
  'tree.addGroupEllipsis',
  'addMenu.nodeIn',
  'addMenu.groupIn',
] as const;
export type AddMenuLabelKey = (typeof ADD_MENU_LABEL_KEYS)[number];

export interface AddMenuTarget {
  /** The folder BOTH actions act on: the new node's `group_id` AND the new group's `parent_id`.
   *  `null` = ungrouped / top level. One field, not two, because in every case they are the same
   *  value — a second field carrying one fact is the copy that drifts. */
  groupId: string | null;
  /** Its name, for the labels. Non-null exactly when `groupId` is non-null. */
  groupName: string | null;
  addNodeKey: AddMenuLabelKey;
  addGroupKey: AddMenuLabelKey;
}

/**
 * Resolve the tree selection to the folder the ＋ menu should file into, and pick the label keys.
 *
 * A selected node contributes its own group, so "add another one next to this" works. The
 * load-bearing rule is the fallback: the target is kept ONLY if it resolves to a group this page
 * can name. A menu item reading "Add node…" that silently files into a folder the operator cannot
 * see is worse than top level, so an unresolvable selection (lazily-unloaded node, filter-mode
 * search page, a `?sel=` pointing at a deleted group) collapses to null. That is what makes
 * `groupName === null` ⟺ `groupId === null` an invariant, and the key choice derivable from it.
 */
export function addMenuTarget(
  selected: TreeSelection,
  groups: NodeGroup[],
  nodes: NodeSummary[],
): AddMenuTarget {
  const wanted = !selected
    ? null
    : selected.kind === 'group'
      ? selected.id
      : (nodes.find((n) => n.id === selected.id)?.group_id ?? null);
  const group = wanted ? (groups.find((g) => g.id === wanted) ?? null) : null;
  return group
    ? {
        groupId: group.id,
        groupName: group.name,
        addNodeKey: 'addMenu.nodeIn',
        addGroupKey: 'addMenu.groupIn',
      }
    : {
        groupId: null,
        groupName: null,
        addNodeKey: 'tree.addNodeEllipsis',
        addGroupKey: 'tree.addGroupEllipsis',
      };
}
