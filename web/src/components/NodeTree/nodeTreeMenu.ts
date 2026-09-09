// SPDX-License-Identifier: AGPL-3.0-only
// Which right-click menus have anything in them, and which rows have a suppression to explain.
//
// 🚨 **This is the rule that shipped an operator a tree with no context menu at all.** It was one
// `if (!canEdit) return;` on the row's `onContextMenu`, and `canEdit` is `ManageConfig` — while the
// maintenance and mute entries inside that menu are `ManageMaintenance` and `AckAlerts`, which an
// operator holds (ADR-057). Closing a mixed menu on its strictest member takes the looser items
// with it, silently, and it read as deliberate because it was *consistent*: an admin saw the menu,
// so nothing looked broken (`ui-conventions.md`, "never gate a mixed menu on its strictest member").
//
// It lived in `NodeTree.tsx`, so no test could reach it. It can now.

import type { SuppressionIndex, SuppressionTarget } from '../../lib/suppression';
import type { NodeGroup, NodeSummary } from '../../types/api';

/** What the caller may do, as the tree sees it. Each field is one permission's answer, already
 *  resolved by the page — this module never looks a role up. */
export interface MenuCapabilities {
  /** `ManageConfig`: reshaping the folder tree (add/edit/delete a group, move a node). */
  canEdit: boolean;
  /** Either suppression control is available — `ManageMaintenance` or `AckAlerts`. */
  canSuppress: boolean;
  /** The "add a monitoring node here" item is wired. */
  canAddNode: boolean;
}

/**
 * Whether a right-click on a **group** row would produce a menu with anything in it.
 *
 * Three independent reasons for the menu to exist, and any one of them is enough. An operator who
 * may open a maintenance window on a folder still gets that half of the menu.
 */
export function groupMenuHasItems(c: MenuCapabilities): boolean {
  return c.canEdit || c.canSuppress || c.canAddNode;
}

/**
 * Whether this folder can offer "run a discovery sweep here" (ADR-100 decision 10).
 *
 * Two conditions, and neither is negotiable. The folder must carry at least one IP prefix — the
 * item's whole content is "sweep these ranges", and without one there is nothing to sweep. And the
 * caller must hold `ManageConfig`, which is what `POST /api/v1/discovery/scan` demands; an item
 * that navigates to a screen the caller is then refused on is worse than no item.
 *
 * ⚠️ **A folder's own prefixes only — never its descendants'.** A Region with twenty sites under it
 * would offer sixty ranges against a 1024-address ceiling, so "run discovery on Japan" could only
 * ever be a spec that refuses to run. In every NetBox seen so far a Region carries none, so this
 * simply does not appear on one.
 *
 * ⚠️ **This does not need a `MenuCapabilities` field**, and that is a deliberate reading of the
 * rule this module exists for rather than an omission. The regression that created this file was a
 * *mixed* menu closed on its strictest member; this item's permission is `canEdit`, which is also
 * "Add subgroup"'s, so a menu containing only this one cannot exist. Adding a field would make
 * `groupMenuHasItems` true in a case where the menu would render empty.
 */
export function canRunDiscovery(group: NodeGroup, c: MenuCapabilities): boolean {
  return c.canEdit && group.prefixes.length > 0;
}

/**
 * Whether a right-click on the **Ungrouped header or the empty tree** would produce a menu.
 *
 * Only one item can live there — "add a node at the top level" — so this is that item's own
 * condition rather than a combination.
 */
export function rootMenuHasItems(c: MenuCapabilities): boolean {
  return c.canAddNode;
}

/**
 * Whether a **node** row's menu is worth opening.
 *
 * Always. "Open" is navigation and needs no permission, so the menu is never empty — which is why
 * the node branch has no gate at the call site either.
 */
export function nodeMenuHasItems(): boolean {
  return true;
}

/**
 * Whether this row currently has anything the release panel could act on or explain.
 *
 * ⚠️ A node counts as suppressed when it is *exempt* too. An exemption is a released suppression
 * that the row still has to be able to explain — "why did this stop being silent" is the same
 * question as "why is it silent", and the panel is the only place either is answered.
 *
 * ⚠️ `state === 'maintenance'` is the engine's rolled-up opinion and lags a release by up to one
 * refresh (~30s), which is why the caller consults it only while the row is not already exempt.
 * Here it is one more reason the panel has something to say, never the only one.
 */
export function hasSuppression(
  suppression: SuppressionIndex | undefined,
  target: SuppressionTarget,
  node?: NodeSummary,
): boolean {
  if (target.kind === 'group') {
    return (
      !!suppression?.maintenanceGroups.has(target.id) || !!suppression?.muteGroups.has(target.id)
    );
  }
  return (
    !!suppression?.maintenanceNodes.has(target.id) ||
    !!suppression?.muteNodes.has(target.id) ||
    !!suppression?.exemptMaintenanceNodes.has(target.id) ||
    !!suppression?.exemptMuteNodes.has(target.id) ||
    node?.state === 'maintenance'
  );
}

/**
 * Whether a node's menu can offer "move to the folder whose IP range contains this address"
 * (ADR-124 決定 6).
 *
 * ⚠️ **Unlike `canRunDiscovery`, this asks about every folder, not the row's own.** The question
 * is "is there anywhere for this node to go", and the answer is spread across the tree: the folder
 * that claims `192.168.1.7` is whichever one carries `192.168.1.0/24`, wherever it sits. The
 * *matching* still happens on the server — this only decides whether the item is worth offering.
 *
 * 🚨 **An empty `prefixes` does not always mean "no ranges".** A scoped caller receives breadcrumb
 * ancestors with their prefixes cleared (`api/groups.rs::visible_groups`), so this can read false
 * for someone the server would in fact have matched. That is the conservative direction — the item
 * stays hidden rather than opening a dialog that then finds nothing — and it is one of the two
 * reasons the containment test is not computed in the browser.
 *
 * ⚠️ **Takes the permission, not the whole `MenuCapabilities`.** The two callers are the tree's
 * own menu and the selection bar on the page above it, and the bar has no menu capabilities to
 * hand over — building one there just to pass `canEdit` would put three `false`s at a call site
 * as decoration.
 */
export function canMoveByPrefix(groups: readonly NodeGroup[], canEdit: boolean): boolean {
  return canEdit && groups.some((g) => g.prefixes.length > 0);
}

/**
 * Whether the node menu should talk about the **working set** rather than this one row.
 *
 * The bulk items appear once at least one node is checked; below that the menu keeps saying
 * "this node", which is what a right-click on an unchecked row means. Gated on `canEdit` because
 * every item it introduces is a move.
 */
export function bulkMenuItems(checkedCount: number, c: MenuCapabilities): boolean {
  return c.canEdit && checkedCount > 0;
}
