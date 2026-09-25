// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree (All nodes). Hierarchical groups (folders) with their member nodes, modelled
// on HoTTY's HostTree: expand/collapse, per-row hover actions, a right-click context menu, and
// HTML5 drag-and-drop.
//
// Every row shares the same leading layout — a fixed-width twisty slot (a real chevron for groups,
// an invisible spacer for nodes and childless groups) then a fixed-width icon slot (the group icon
// or, for a node, its status dot). Indentation is purely `depth × INDENT`, so a child's icon lines
// up one step in from its parent's and names sit in a clean column.
//
// Drag-and-drop supports both moving AND reordering (HoTTY-style): the drop position is read from
// the cursor's vertical position within the target row — the top/bottom edge means "before/after"
// (reorder among siblings), the middle of a group means "inside" (nest / assign). Dropping onto
// "Ungrouped" moves a node to the root / a group to the top level. Group moves that would nest a
// group inside its own subtree are refused (cycle guard). This component is presentation +
// interaction only; the page owns the data and turns the callbacks into API calls + a reload.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { useTranslation } from 'react-i18next';
import type { NodeGroup, NodeSummary, PoolOption } from '../../types/api';
import { poolChoices, sharedOwnPool } from '../../lib/pool';
import { targetNodeCount, type ActionTarget } from '../../lib/actionTarget';
import { nodeBadges } from '../../lib/nodeKind';
import { NodeBadgeTag } from '../ui/NodeBadgeTag';
import { brandBadgeClass } from '../../lib/brandBadge';
import { GROUP_ORIGIN_BADGE_BRANDS, GROUP_ORIGIN_BADGES, groupOriginOf } from '../../lib/groupOrigin';
import {
  asGroupType,
  buildNodeTree,
  filterCollapsedFrom,
  filterTerm,
  flattenTree,
  flatRowKey,
  pendingGroupKeys,
  pressTwisty,
  sameNameNodeIds,
  shouldForgetTouched,
  touchedFor,
  treeFilterKey,
  type FlatRow,
  type StateCounts,
  type TreeGroup,
} from '../../lib/nodeTree';
import { useDebouncedValue } from '../../lib/useDebouncedValue';
import { usePrefsStore } from '../../prefs';
import { setNodeTreeCollapsed } from '../../serverPrefs';
import { useTreeTouchedStore } from '../../store';
import {
  DURATION_PRESETS,
  type ReleaseAction,
  type SuppressionIndex,
  type SuppressionPanelRow,
  type SuppressionTarget,
} from '../../lib/suppression';
import { formatScheduleTime } from '../../lib/format';
import { StatusDot } from '../ui/StatusDot';
import { Button } from '../ui/Button';
import { ActionMenu } from '../ui/ActionMenu';
import { AnchoredPopover } from '../ui/AnchoredPopover';
import { WrenchIcon, BellIcon, BellOffIcon, PinIcon } from '../ui/icons';
import { nothingPinned, type PinnedView } from '../../lib/pins';
import { HealthBar } from '../HealthBar/HealthBar';
import {
  dragPreview,
  dropAction,
  dropToPerform,
  dropAllowed,
  dropParentId,
  dropPosition,
  nodeDragItem,
  rootDropAction,
  withDropSlot,
  type DragItem,
  type DropAction,
  type DropFeedback,
  type DropPos,
  type Target,
} from './nodeTreeDnd';
import {
  canMoveByPrefix,
  canRunDiscovery,
  groupMenuHasItems,
  hasSuppression,
  nodeActionItems,
  nodeDeleteItems,
  nodeMoveItems,
  rootMenuHasItems,
  type MenuCapabilities,
} from './nodeTreeMenu';
import { clickOutcome, rowNode, toggleChecked, type CheckedNodes } from './nodeTreeSelect';
import {
  anchorOnSettle,
  ariaLevel,
  CURSOR_SETTLE_MS,
  cursorAfterSelection,
  cursorForMove,
  indexOfSelection,
  keyBelongsToTree,
  menuStep,
  moveCheckedChange,
  pageRows,
  rowDomId,
  rowSelection,
  settleCursor,
  shouldRefocusTree,
  spaceCheckedChange,
  treeKeyAction,
  type MoveGesture,
} from './nodeTreeKeys';
import { pinFocusScroll, restoreScroll, type ScrollAt } from './nodeTreeScroll';
import {
  cellTones,
  checkedPerGroup,
  checkedRowIndices,
  litGuides,
  parentRows,
  stickyParents,
  treeGuides,
  type GuideKind,
} from './nodeTreeGuides';
import { GroupIcon } from './GroupIcon';
import './NodeTree.css';

/** What the inventory currently has selected (drives the split's detail pane). */
export type TreeSelection = { kind: 'node' | 'group'; id: string } | null;

/** Pixels of indent per tree depth. */
const INDENT = 16;
/** Left padding of a depth-0 row. */
const BASE_PAD = 6;
/** Where a branch line runs inside its level's column: the middle of the 16px twisty slot, so a
 *  folder's line drops straight down from under its own chevron (ADR-171). */
const GUIDE_X = 8;
/** Fixed row height (matches `--row-h` in tokens.css) — every tree row is one line, so the
 *  flattened list virtualizes with a uniform estimate (S13). */
const ROW_H = 30;
/** How far in from a row's left edge a menu opened from the keyboard appears (ADR-155 決定 7) — past
 *  the twisty and the icon, where a right-click on the name would have put it. */
const MENU_KEY_INSET_PX = 40;

/** How long the viewport must settle before its folders are fetched (ADR-125).
 *
 *  Deliberately NOT `SEARCH_DEBOUNCE_MS` (200ms): that one is tuned to a person typing, and here the
 *  operator is looking at a tree waiting for it to fill in. Short enough to feel immediate, long
 *  enough that a momentum scroll queues the folders it lands on rather than every one it passes. */
const PENDING_SETTLE_MS = 100;

/** One shared empty working set, so a tree rendered without `checked` reads from a stable value
 *  rather than allocating a new `Map` on every render (which would defeat every memo below it). */
const EMPTY_CHECKED: CheckedNodes = new Map();

/** The same trick for the folders "Folders with nodes only" keeps regardless (ADR-159): a page that
 *  has created none passes nothing, and a fresh `Set` per render would rebuild the flat list. */
const NO_KEPT_GROUPS: ReadonlySet<string> = new Set();

/** What the tree is telling the operator about a drag in flight. The shape lives in
 *  `nodeTreeDnd.ts`, beside the judgements that read it and the tests that can run them — and since
 *  ADR-162 増分 2 it carries the whole `Target`, because the slot row replays it (see `DropFeedback`). */
type DropTarget = DropFeedback | null;
type Menu =
  | { x: number; y: number; kind: 'group'; group: TreeGroup }
  | { x: number; y: number; kind: 'node'; node: NodeSummary }
  | { x: number; y: number; kind: 'root' }
  // Opened by clicking a suppression marker: what is silencing this row, and what can be done
  // about it. A `Menu` variant rather than its own popover so it shares the one context menu —
  // its point, its dismissal and, since ADR-124 Inc.2, its portal and its clamping to the viewport.
  | { x: number; y: number; kind: 'suppress'; target: SuppressionTarget; node?: NodeSummary }
  | null;

interface Props {
  groups: NodeGroup[];
  nodes: NodeSummary[];
  canEdit: boolean;
  /** Per-group DIRECT member state counts (server rollup, A-1). When given, group rows roll up from
   *  these — correct over the whole fleet even before a group's members are lazily loaded (A-3). */
  groupCounts?: Record<string, StateCounts>;
  /** The counts above have been requested and have not arrived (ADR-133). The tree paints from the
   *  folder list alone and starts fetching members immediately; the bars and pills fill in when the
   *  rollup lands. Without it a skeleton with no counts asks for no members at all — the reason is
   *  on `flattenTree`'s `countsPending`, which is where the decision lives. */
  countsPending?: boolean;
  /** Ids of groups whose members have been lazily fetched (A-3). An open group not in this set shows
   *  a loading placeholder instead of its members. Omit (with `groupCounts`) ⇒ every group loaded. */
  loadedGroups?: Set<string>;
  /** Filter mode only: ids of groups whose whole membership is being fetched because the term
   *  matched the group's own NAME (`revealedGroupKeys`). Only these can show a loading row while
   *  filtering — every other group is showing the search page's hits and nothing more. */
  revealedGroups?: Set<string>;
  /** Ids of groups whose member fetch failed (ADR-125). They show a failed row with a retry
   *  control rather than a placeholder that never resolves. */
  failedGroups?: Set<string>;
  /** Fetch one failed group's members again. The only retry there is — a failed group is never
   *  re-fetched on its own, because doing that automatically is what produced an unbounded loop. */
  onRetryGroup?: (groupId: string) => void;
  /** The folders currently ON SCREEN and still waiting for their members (ADR-125). This is what
   *  makes the fetch follow the viewport the way the rendering already does; the page hands it
   *  straight to the member cache. Called only when the set actually changes. */
  onPendingGroupsChange?: (groupIds: string[]) => void;
  /** First inventory load in flight — show a loading placeholder, not the empty message. */
  loading?: boolean;
  /** Currently-selected row (highlighted with the inset accent bar); drives the split detail pane. */
  selected?: TreeSelection;
  /** Select a node/group row (single-click). Falls back to `onOpenNode` when not provided. */
  onSelectNode?: (node: NodeSummary) => void;
  onSelectGroup?: (group: NodeGroup) => void;
  /** Clear the selection (ADR-073). Two gestures reach it: clicking the already-selected row, and
   *  clicking the empty space below the rows. Omit and both become no-ops — the tree keeps its
   *  pre-ADR-073 behaviour of only ever moving the selection, never removing it. */
  onSelectNone?: () => void;
  /** Case-insensitive name filter; non-empty hides non-matching rows and opens every folder
   *  (the operator can still close one while it is on — ADR-053 Inc.11). */
  filter?: string;
  /** The nodes handed in were already narrowed by the pane's state / kind / pool controls, which
   *  run server-side. The tree cannot see those filters, so without this it does not know it is
   *  filtering at all: every folder stays on screen — including the ones with nothing matching
   *  under them — and a collapsed folder stays collapsed over its own matches. */
  narrowed?: boolean;
  /** The VALUES behind `narrowed` (`inventoryFilters.ts::inventoryKey`). A folder collapsed under
   *  one filter must not stay collapsed under the next, and `narrowed` cannot tell Critical from
   *  Warning. */
  narrowKey?: string;
  /** The page has asked for a search — a term typed or restored from `?q=`, or a state / kind / pool
   *  filter — whether or not it has reached `filter` yet (ADR-154 decision 11).
   *
   *  🚨 **`filter` lags it, and on a reload the lag is the whole first render**: the page reads `?q=`
   *  at once, while `filter` is set by the search hook's effect. The tree forgets which folders were
   *  pressed under a search only when neither this nor its own filtering says a search is on — ask
   *  `filter` alone and a reload erases the record the moment it is restored. Omit it where there is
   *  no search to wait for; the tree then decides from `filter` and `narrowed` alone. */
  searchRequested?: boolean;
  /** Render the internal Add-group / drag-hint toolbar (the split hosts Add-group in its pane head). */
  showToolbar?: boolean;
  onOpenNode: (node: NodeSummary) => void;
  onAddGroup: (parentId: string | null) => void;
  onEditGroup: (group: NodeGroup) => void;
  onDeleteGroup: (group: NodeGroup) => void;
  /** Right-click → edit a node (its check, profile, credential, identity, pool) — the same dialog
   *  the detail pane's "Edit node" opens, reachable without selecting the row first. Omit to hide
   *  the menu item. */
  onEditNode?: (node: NodeSummary) => void;
  /** Right-click → add a monitoring node, placed into `groupId` (`null` = top level / Ungrouped).
   *  The manual, Discovery-free way to add a target. Omit to hide the menu item. */
  onAddNode?: (groupId: string | null) => void;
  /** Right-click → delete a node (opens a destructive-consent modal). Omit to hide the item. */
  onDeleteNode?: (node: NodeSummary) => void;
  /** Delete every checked node — the menu's item when the right-clicked row is in the working set
   *  (ADR-124 増分 6). Omit to fall back to deleting the row. */
  onDeleteChecked?: () => void;
  /** Open the "move node" picker (context-menu / button path, keyboard-accessible). */
  onRequestMoveNode: (node: NodeSummary) => void;
  /** The working set — nodes checked with Ctrl / Shift for a bulk action (ADR-124 決定 2).
   *  Held by the page, never in the URL. */
  checked?: CheckedNodes;
  /** Row a Shift click measures its range from. An **id**: the flat row list is rebuilt on every
   *  SSE frame and filter change, so an index would point at whatever sits there now. */
  anchorId?: string | null;
  /** The working set changed. **Omit to disable Ctrl / Shift entirely** — which is what a caller
   *  without ManageConfig does, so a viewer cannot assemble a batch with nowhere to send it. */
  onCheckedChange?: (next: Map<string, NodeSummary>, anchorId: string | null) => void;
  /** Move every checked node (the menu's bulk item). Omit to hide it. */
  onMoveChecked?: () => void;
  /** Propose folders for every checked node by IP range. Omit to hide it. */
  onMoveCheckedByPrefix?: () => void;
  /** Tag every checked node (the menu's bulk item). Omit to hide it. */
  onTagChecked?: () => void;
  /** Propose a folder for this one node by IP range. Omit to hide it. */
  onMoveNodeByPrefix?: (node: NodeSummary) => void;
  /** Move nodes into a group (or null = ungroup) — a drop, of one row or of the whole working set.
   *  **A list even for one** (ADR-124 Inc.4): the drag used to hand over a single id and the page
   *  used to answer it with the single-node endpoint, which is how a three-row selection moved one
   *  node.
   *
   *  `placement` names the sibling node to land next to; omitted appends to the end of the folder.
   *  🚨 **A batch carries it too since 増分 8** — there used to be a separate `onReorderNode` that
   *  took one id, so dropping several nodes between two rows silently appended them instead. */
  onMoveNodes: (
    nodeIds: readonly string[],
    groupId: string | null,
    placement?: { before?: string; after?: string },
  ) => void;
  /** Re-parent a group (or null = top level), appending it — drop into a group / onto Ungrouped. */
  onMoveGroup: (groupId: string, parentId: string | null) => void;
  /** Arrange one folder's **direct** children in name order (ADR-130, amended by ADR-162).
   *  Subfolders and member nodes are renumbered as one list, so a subfolder named `m` lands between
   *  the nodes named `k` and `p`; folders deeper down are untouched. 🚨 **Overwrites a
   *  hand-arranged order with no undo** — by decision, and the reason no confirmation is asked is
   *  that the caller right-clicked the folder this acts on. Gated on `canEdit` at the item, never
   *  at the menu (see `nodeTreeMenu.ts`). */
  onSortGroupChildren: (groupId: string, direction: 'asc' | 'desc') => void;
  /** Drag-reorder a group next to a sibling row (before/after) under a parent. ⚠️ Since ADR-162 the
   *  sibling may be a **node**: under one parent the two kinds are one ordered list. */
  onReorderGroup: (
    groupId: string,
    dest: { parentId: string | null; before?: string; after?: string },
  ) => void;
  /** Which nodes/groups are currently in maintenance or muted (drives the per-row markers). */
  suppression?: SuppressionIndex;
  /** What the release panel shows for a row — resolved by the page, which holds the window, mute
   *  and exemption lists. Omit and the markers stay decorative, as they were before the panel. */
  suppressionRows?: (target: SuppressionTarget, node?: NodeSummary) => SuppressionPanelRow[];
  /** Act on a suppression from the panel or the context menu. One callback over a union so the
   *  page answers with a single exhaustive switch. Omit to hide every release control. */
  onRelease?: (action: ReleaseAction) => void;
  /** Right-click → put a node, folder or the working set into maintenance. `durationMs` = preset
   *  length from now; `null` = open the full create form prefilled with the scope ("Custom…").
   *
   *  🚨 **The target is an `ActionTarget` since ADR-124 増分 11**: it was a single row, so a preset
   *  pressed with a dozen nodes selected covered exactly one of them. */
  onSetMaintenance?: (target: ActionTarget, durationMs: number | null) => void;
  /** Right-click → mute a node/group. `durationMs`/`null` as for `onSetMaintenance`. */
  onSetMute?: (target: ActionTarget, durationMs: number | null) => void;
  /** Pools offered by the right-click poll-pool chips (`GET /api/v1/pools`). */
  pools?: PoolOption[];
  /** Right-click → assign a node, a folder, or the whole working set to a poll-pool. A pool name
   *  sets it, `''` clears it back to inherited, and `null` opens the Custom… dialog — the same
   *  convention as the suppression chips above.
   *
   *  🚨 **The target is an `ActionTarget` since ADR-124 増分 10**: it was a single row, so the
   *  chips wrote one node while sitting in a menu headed "Move 20 selected…". */
  onSetPool?: (target: ActionTarget, pool: string | null) => void;
  /** Right-click → aim a discovery sweep at this folder's IP prefixes (ADR-100 decision 10).
   *  Shown only for a folder that carries some — see `canRunDiscovery`. Omit to hide the item. */
  onRunDiscovery?: (group: NodeGroup) => void;
  /** The signed-in account's pins (ADR-146): the pin marks, and — with `pinnedOnly` — which rows
   *  are kept. Omit while pins are unavailable (a core without the endpoint, or not loaded yet). */
  pins?: PinnedView;
  /** Show only what `pins` keeps: pinned folders whole, pinned nodes, and the folders above both. */
  pinnedOnly?: boolean;
  /** Hide folders with no node anywhere below them (ADR-159). */
  withNodesOnly?: boolean;
  /** Folders `withNodesOnly` keeps whatever their membership — the ones created on this screen
   *  since it was opened, which are empty by definition (ADR-159). Ignored while the switch is
   *  off; the tree never adds to it. */
  keepGroups?: ReadonlySet<string>;
  /** Right-click → poll this node, or the whole working set, immediately (ADR-124 増分 12).
   *  Omit to hide the item — it is `ManageConfig`, the permission its handler checks. */
  onPollNodes?: (target: ActionTarget) => void;
  /** Right-click → pin or unpin a node or folder. Omit to hide the item. */
  onTogglePin?: (target: { kind: 'node' | 'group'; id: string }) => void;
}

export function NodeTree({
  groups,
  nodes,
  canEdit,
  groupCounts,
  countsPending,
  loadedGroups,
  revealedGroups,
  failedGroups,
  onRetryGroup,
  onPendingGroupsChange,
  loading,
  selected,
  onSelectNode,
  onSelectGroup,
  onSelectNone,
  filter,
  narrowed,
  narrowKey,
  searchRequested,
  showToolbar = true,
  onOpenNode,
  onAddGroup,
  onEditGroup,
  onDeleteGroup,
  onEditNode,
  onAddNode,
  onDeleteNode,
  onDeleteChecked,
  onRequestMoveNode,
  checked,
  anchorId,
  onCheckedChange,
  onMoveChecked,
  onMoveCheckedByPrefix,
  onTagChecked,
  onMoveNodeByPrefix,
  onMoveNodes,
  onMoveGroup,
  onSortGroupChildren,
  onReorderGroup,
  suppression,
  suppressionRows,
  onRelease,
  onSetMaintenance,
  onSetMute,
  pools,
  onSetPool,
  onRunDiscovery,
  pins,
  pinnedOnly,
  withNodesOnly,
  keepGroups,
  onPollNodes,
  onTogglePin,
}: Props) {
  const { t } = useTranslation('nodes');
  const tree = useMemo(() => buildNodeTree(groups, nodes), [groups, nodes]);
  // Rows that would read identically by name alone get their address beside it (ADR-139 増分 2).
  const sameName = useMemo(() => sameNameNodeIds(nodes), [nodes]);
  // Expansion defaults to fully-expanded and persists across reloads and machines: the prefs store
  // keeps the set of groups the user explicitly collapsed (empty ⇒ everything open), `serverPrefs.ts`
  // carries it to the account (ADR-154), so the last layout is restored and any newly-added group
  // shows expanded automatically.
  const collapsed = usePrefsStore((s) => s.nodeTreeCollapsed);
  const [drag, setDrag] = useState<DragItem | null>(null);
  const [dropTarget, setDropTarget] = useState<DropTarget>(null);
  const [menu, setMenu] = useState<Menu>(null);
  /** Group row whose ＋ menu is open — keeps that row's hover-revealed actions on screen. */
  const [addMenuGroup, setAddMenuGroup] = useState<string | null>(null);
  /** Where the keyboard has put the selection before `?sel=` has been written (ADR-155 決定 3).
   *  Null outside that window. The rows draw `shown`, so a held arrow key moves `.sel` at once
   *  while the URL — and the detail pane keyed on it — waits for the keys to rest. */
  const [cursor, setCursor] = useState<TreeSelection>(null);
  const shown: TreeSelection = cursor ?? selected ?? null;
  /** The selection this tree last wrote to `?sel=`, from a key or a click. Two readers: the settle
   *  effect, which re-runs on every render and must not write the same press again while the URL
   *  catches up; and the selection effect, which tells "the URL caught up with us" apart from
   *  "something else moved the selection" (`cursorAfterSelection`). */
  const committed = useRef<TreeSelection>(null);

  // Active name filter (case-insensitive). While filtering, every group starts open and
  // non-matching rows are hidden, so matches are always revealed — and a group matched by its own
  // name reveals its whole subtree, members included.
  const q = filterTerm(filter ?? '');
  // Pinned only narrows the tree as well (ADR-146).
  const pinnedFilter = pinnedOnly ? pins : undefined;
  // …and so does Folders with nodes only (ADR-159). Built through a memo because `flattenTree`'s
  // option is an object: a fresh literal every render would rebuild the whole flat list on every
  // render, which is the cost `liveTreeNodes` and the memo below exist to avoid.
  const withNodes = useMemo(
    () => (withNodesOnly ? { keep: keepGroups ?? NO_KEPT_GROUPS } : undefined),
    [withNodesOnly, keepGroups],
  );
  const filtering = q.length > 0 || narrowed === true || pinnedFilter !== undefined;
  // A term or a state / kind / pool filter — the questions whose matches a closed folder must not
  // hide. ⚠️ Not Pinned only on its own: that one browses the saved layout (ADR-154 decision 7).
  const searching = q.length > 0 || narrowed === true;
  // The folders pressed under THIS filter (ADR-053 Inc.11, reshaped by ADR-154). The press itself
  // wrote the saved layout; this set is only what lets the filtered tree show that folder closed.
  // Kept in the tab's storage (ADR-154 increment 2), so a reload under the same `?q=` still shows
  // the folder closed.
  const touched = useTreeTouchedStore((s) => s.touched);
  // Leaving the filter forgets it, so typing the same term again starts open too — and the saved
  // layout keeps what was pressed. 🚨 Both halves of `shouldForgetTouched`, never `searching` alone:
  // on the first render after a reload `filter` is still empty while the page already holds `?q=`.
  // An effect is fine here, unlike increment 1's in-render reset: the record is read only while
  // searching, and forgetting only happens while not, so the one late frame never reaches the screen.
  const forgetTouched = shouldForgetTouched(searching, searchRequested === true);
  useEffect(() => {
    if (forgetTouched) useTreeTouchedStore.getState().forget();
  }, [forgetTouched, touched]);
  const collapseKey = treeFilterKey(
    filter ?? '',
    narrowed === true,
    narrowKey ?? '',
    pinnedFilter !== undefined,
  );
  const touchedIds = touchedFor(touched, collapseKey);
  const filterCollapsed = useMemo(
    () => filterCollapsedFrom(collapsed, touchedIds),
    [collapsed, touchedIds],
  );
  // The flattened, display-ordered list of visible rows — the single source of truth the virtualized
  // body renders (collapse state + filter applied). Only the on-screen window is turned into DOM, so
  // a tens-of-thousands-node inventory stays responsive (S13).
  const flat = useMemo(
    () =>
      flattenTree(tree, {
        collapsed,
        filterCollapsed,
        filter: filter ?? '',
        narrowed,
        groupCounts,
        countsPending,
        loadedGroups,
        revealedGroups,
        failedGroups,
        pinned: pinnedFilter,
        withNodesOnly: withNodes,
      }),
    [
      tree,
      collapsed,
      filterCollapsed,
      filter,
      narrowed,
      groupCounts,
      countsPending,
      loadedGroups,
      revealedGroups,
      failedGroups,
      pinnedFilter,
      withNodes,
    ],
  );
  /**
   * The rows actually drawn: `flat`, plus the one insertion slot a drag in flight would land in
   * (ADR-162 増分 2).
   *
   * 🚨 **Everything that turns a row index into something is measured against THIS list, never
   * `flat`.** The two differ by one row while a drag is over a droppable row, and an index taken
   * from one and spent on the other is off by one *silently* — the visible symptom would be a
   * loading placeholder asking for a different folder's members than the one it is drawn under.
   * `flat` survives as the input to this memo and nowhere else.
   *
   * ⚠️ When nothing is being dragged `withDropSlot` returns `flat` itself, so an idle tree renders
   * from exactly the array `flattenTree` produced and this whole increment costs it nothing.
   */
  const drawn = useMemo(() => withDropSlot(flat, dropTarget), [flat, dropTarget]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const rowVirtualizer = useVirtualizer({
    count: drawn.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_H,
    overscan: 16,
    getItemKey: (index) => flatRowKey(drawn[index]),
  });
  // Row click selects (drives the split detail pane); without a select handler, fall back to the
  // legacy "open node" behaviour so the tree still works on its own.
  //
  // Clicking the row that is already selected clears it instead (ADR-073 decision 1). Both the row
  // and its name button funnel through here, so one place covers both. This is the gesture that
  // works when the others cannot: the blank-space click needs blank space, which a tree taller than
  // its pane does not have, and Escape needs a keyboard.
  const isSelected = (kind: 'node' | 'group', id: string) =>
    selected?.kind === kind && selected.id === id;
  // A click overtakes a key press still waiting to be written (ADR-155 決定 3). While one is pending
  // the rows show the cursor, not `?sel=`, so a click on the row `?sel=` holds is "go back there" —
  // it drops the cursor rather than clearing a selection the operator could not see was current.
  //
  // 🚨 **A click shows its row at once, through the same cursor a key uses.** The URL write is
  // rendered as a transition, so for a moment after a click `selected` still names the previous row
  // — and a key pressed in that moment started from it. Measured in the full Tier1 run: a click on a
  // folder followed at once by Down selected the first row of the tree (the tree read "nothing
  // selected", and Down from nothing is the first row). Marking the click as written is what lets the
  // URL catching up clear the cursor, and stops the settle effect writing it a second time.
  const showClicked = (sel: NonNullable<TreeSelection>) => {
    const next = cursorForMove(sel, selected ?? null, committed.current);
    if (next) committed.current = next;
    setCursor(next);
  };
  const selectNode = (node: NodeSummary) => {
    if (cursor) {
      setCursor(null);
      if (isSelected('node', node.id)) return;
    } else if (onSelectNone && isSelected('node', node.id)) return onSelectNone();
    if (!onSelectNode) return onOpenNode(node);
    showClicked({ kind: 'node', id: node.id });
    onSelectNode(node);
  };
  const selectGroup = (group: NodeGroup) => {
    if (cursor) {
      setCursor(null);
      if (isSelected('group', group.id)) return;
    } else if (onSelectNone && isSelected('group', group.id)) return onSelectNone();
    if (!onSelectGroup) return;
    showClicked({ kind: 'group', id: group.id });
    onSelectGroup(group);
  };

  const checkedNodes: CheckedNodes = checked ?? EMPTY_CHECKED;

  // The branch lines (ADR-171). Every judgement is in `nodeTreeGuides.ts`; these memos only feed it.
  // Measured against `drawn`, like everything else that turns an index into something.
  const guides = useMemo(() => treeGuides(drawn), [drawn]);
  const parents = useMemo(() => parentRows(drawn), [drawn]);
  const shownIndex = useMemo(() => indexOfSelection(drawn, shown), [drawn, shown]);
  const lit = useMemo(
    () => litGuides(drawn, parents, shownIndex, checkedRowIndices(drawn, checkedNodes)),
    [drawn, parents, shownIndex, checkedNodes],
  );
  /** "N selected" on each folder the working set reaches into — the one mark a closed folder can
   *  carry, since its rows (and so their branches) are not drawn (ADR-171 決定 4). */
  const pickCounts = useMemo(() => checkedPerGroup(checkedNodes, groups), [checkedNodes, groups]);
  /** The folders pinned at the top (ADR-171 決定 5). None while dragging: a band over the rows would
   *  hide the row a drop is aimed at. The virtualizer re-renders as the scroll crosses rows, which
   *  is the only granularity the band changes at. */
  const band = drag ? [] : stickyParents(drawn, parents, rowVirtualizer.scrollOffset ?? 0, ROW_H);
  // 🚨 A row the keyboard moves to must not land UNDER the band. `scrollToIndex` treats a row
  // within `scrollPaddingStart` of the top as out of view, and the band's height is only known
  // here, after the virtualizer was built for this render — so it is written onto the options the
  // virtualizer will read when the next key press scrolls. `useVirtualizer` resets its options on
  // every render, so this never outlives the render that computed it.
  rowVirtualizer.options.scrollPaddingStart = band.length * ROW_H;

  /** Apply what a click on a node row decided (ADR-124 決定 2/4 + 増分 1).
   *
   *  🚨 **The decision itself is in `nodeTreeSelect.ts` and must stay there.** This function held
   *  it once, and that is exactly how the two-click Shift range shipped broken: Vitest never loads
   *  a `.tsx`, so the branch that failed to set the anchor was the one branch no test could run,
   *  while `rangeChecked` — handed an anchor by the test itself — passed every case.
   *
   *  Ctrl / Shift never touch `?sel=`, so the pane keeps showing whatever was open while a batch
   *  is assembled. */
  const clickNode = (e: React.MouseEvent, node: NodeSummary) => {
    if (!onCheckedChange) return selectNode(node);
    const outcome = clickOutcome(e, node, {
      flat: drawn,
      anchorId: anchorId ?? null,
      // Passed whole: whether a folder selection may start a batch is decided in the `.ts`.
      selection: selected ?? null,
      checked: checkedNodes,
    });
    if (outcome.checked) onCheckedChange(outcome.checked, outcome.anchorId);
    if (outcome.select) selectNode(node);
  };

  // Whether a right-click on each row kind would produce a menu with anything in it.
  //
  // 🚨 **These used to be one `if (!canEdit) return;`**, and that shipped an operator a tree with
  // no context menu at all — `canEdit` is `ManageConfig`, while the maintenance and mute entries
  // inside the menu are `ManageMaintenance` and `AckAlerts`, which an operator holds (ADR-057).
  // Closing a mixed menu on its strictest member takes the looser items with it, silently. It read
  // as deliberate because it was *consistent*: an admin saw the menu, so nothing looked broken.
  //
  // A node row always has one item — Open, which is navigation and needs no permission — so its
  // menu always opens. The other two are conditional, because an empty menu is worse than none.
  const caps: MenuCapabilities = {
    canEdit,
    canSuppress: !!onSetMaintenance || !!onSetMute,
    canAddNode: !!onAddNode,
    canPin: !!onTogglePin,
  };

  // The suppression markers (maintenance wrench + mute bell-off) shown on a row when active, plus
  // the dashed-outline variant for a node released from a suppression it inherited. Each is a
  // button: clicking one opens the panel below, which is the only place in the UI that answers
  // "why is this row silent".
  const suppressionMarks = (m: {
    target: SuppressionTarget;
    node?: NodeSummary;
    maint: boolean;
    muted: boolean;
    releasedMaint?: boolean;
    releasedMute?: boolean;
  }): React.ReactNode => {
    if (!m.maint && !m.muted && !m.releasedMaint && !m.releasedMute) return null;
    const open = (e: React.MouseEvent) => {
      e.preventDefault();
      // Without this the document-level closer installed below runs in the same click and shuts
      // the panel on the frame it opened.
      e.stopPropagation();
      setMenu({ x: e.clientX, y: e.clientY, kind: 'suppress', target: m.target, node: m.node });
    };
    const mark = (cls: string, title: string, icon: React.ReactNode) => (
      <button type="button" className={`ntree-supp-icon ${cls}`} title={title} onClick={open}>
        {icon}
      </button>
    );
    return (
      <span className="ntree-supp">
        {m.maint && mark('maint', t('tree.suppression.markMaint'), <WrenchIcon />)}
        {m.muted && mark('mute', t('tree.suppression.markMute'), <BellOffIcon />)}
        {m.releasedMaint &&
          mark('maint released', t('tree.suppression.markReleasedMaint'), <WrenchIcon />)}
        {/* A plain bell, not a struck-through bell-off: the mute glyph is *already* a negation, so
            negating it again gave two icons that differ by one faint diagonal at 16px and were
            reported as indistinguishable. Un-slashing says "this node rings again" outright. */}
        {m.releasedMute &&
          mark('mute released', t('tree.suppression.markReleasedMute'), <BellIcon />)}
      </span>
    );
  };

  /** The panel a marker click opens: every suppression on this row, and what can be done to it.
   *  Which blocks appear and what each one offers is decided by `lib/suppression`; this renders
   *  what it returns. That split is not cosmetic — Vitest never loads a `.tsx`, so a judgement made
   *  here is a judgement nothing tests, and the first version of this panel got exactly that wrong
   *  (a released node was offered a release it already had). */
  const suppressionPanel = (m: Extract<Menu, { kind: 'suppress' }>): React.ReactNode => {
    const act = (a: ReleaseAction) => {
      onRelease?.(a);
      setMenu(null);
    };
    const rows = suppressionRows?.(m.target, m.node) ?? [];
    return (
      <div className="ntree-supp-panel">
        {rows.length === 0 ? (
          // Reachable as a race — the window ended between the render that lit the marker and the
          // click. Saying so beats an empty box.
          <div className="ntree-supp-cause">
            <div className="ntree-supp-note">{t('tree.suppression.none')}</div>
          </div>
        ) : (
          rows.map((r) => {
            // Bound before the closure: TypeScript drops a property narrowing inside a callback.
            const control = r.action;
            return (
              <div className="ntree-supp-cause" key={r.key}>
                <div className="ntree-supp-head">{t(r.headKey)}</div>
                {r.title && <div className="ntree-supp-title">{r.title}</div>}
                {(r.labelKey || r.endsAt) && (
                  <div className="ntree-supp-meta">
                    {r.labelKey && t(r.labelKey, r.labelParams)}
                    {r.labelKey && r.endsAt && ' · '}
                    {r.endsAt &&
                      t('tree.suppression.until', { time: formatScheduleTime(r.endsAt) })}
                  </div>
                )}
                {/* Not gated on `canEdit`: that is ManageConfig (editing the inventory), and
                    ending a window or lifting a mute is not. The page decides — it withholds
                    `onRelease` entirely when the caller may release neither kind, and strips the
                    action from the blocks of the kind they may not (`releasableRows`). */}
                {onRelease && control && (
                  <div className="ntree-supp-act">
                    <button type="button" onClick={() => act(control.action)}>
                      {t(control.labelKey)}
                    </button>
                  </div>
                )}
                {onRelease && !control && r.noteKey && (
                  <div className="ntree-supp-note">{t(r.noteKey, r.noteParams)}</div>
                )}
              </div>
            );
          })
        )}
      </div>
    );
  };

  // The Maintenance/Mute quick-duration section appended to a row's context menu. A preset fires
  // immediately (now + length); "Custom…" opens the full create form prefilled with the scope.
  //
  // Releasing is *not* a fourth chip beside them. It switches this menu to the panel at the same
  // coordinates, so the decision of what a release does to each cause lives in exactly one place —
  // and so a mis-aimed click lands on a panel that names what it would release rather than on an
  // action. The chips create suppression, which is safe to get wrong; releasing is what makes a
  // fleet page during planned work.
  // 🚨 **Since ADR-124 増分 11 the presets act on the working set when the row carries it.** Until
  // then they wrote one node while sitting in a menu headed "Move 20 selected…" — so an operator
  // silencing a dozen devices for tonight's work covered exactly one, and found out by being paged.
  // The scope is `nodeActionItems`, the same answer the moves, Delete and the pool chips read.
  //
  // ⚠️ **The release panel stays about the row.** It is opened from a row's own marker and names
  // what it would release; a batch release would have to reconcile causes that differ per node.
  const suppressionMenu = (
    target: ActionTarget,
    rowTarget: SuppressionTarget,
    node: NodeSummary | undefined,
    at: { x: number; y: number },
  ): React.ReactNode => {
    if (!onSetMaintenance && !onSetMute) return null;
    const many = target.kind === 'nodes';
    const count = targetNodeCount(target);
    const row = (
      label: string,
      manyLabel: string,
      handler: (t: ActionTarget, ms: number | null) => void,
    ) => (
      <div className="ntree-menu-section">
        <div className="ntree-menu-label">{many ? manyLabel : label}</div>
        <div className="ntree-menu-durs">
          {DURATION_PRESETS.map((p) => (
            <button
              type="button"
              key={p.label}
              className="ntree-dur"
              onClick={() => {
                handler(target, p.ms);
                setMenu(null);
              }}
            >
              {p.label}
            </button>
          ))}
          <button
            type="button"
            className="ntree-dur"
            onClick={() => {
              handler(target, null);
              setMenu(null);
            }}
          >
            {t('tree.custom')}
          </button>
        </div>
      </div>
    );
    return (
      <>
        <div className="ntree-menu-sep" />
        {onSetMaintenance &&
          row(t('tree.maintenance'), t('tree.maintenanceSelected', { count }), onSetMaintenance)}
        {onSetMute && row(t('tree.mute'), t('tree.muteSelected', { count }), onSetMute)}
        {/* ⚠️ Asks about `rowTarget`, never `target`: the presets above may be acting on the
            batch, but a release is about what is suppressing *this* row and names it. */}
        {onRelease && hasSuppression(suppression, rowTarget, node) && (
          <button
            type="button"
            onClick={() =>
              setMenu({ x: at.x, y: at.y, kind: 'suppress', target: rowTarget, node })
            }
          >
            {t('tree.suppression.act.open')}
          </button>
        )}
      </>
    );
  };

  // The poll-pool chip section appended to a row's context menu (ADR-009/020). A pool chip assigns
  // immediately, "Inherit" clears the target's own pool, and "Custom…" opens the dialog for a pool
  // that doesn't exist yet. Same shape and same immediate-write behaviour as `suppressionMenu`.
  //
  // `currentPool` is the target's OWN pool (`null` ⇒ inherited), which is exactly what these chips
  // write — so it is what marks the active one.
  //
  // 🚨 **Since ADR-124 増分 10 these chips act on the working set when the row carries it.** Until
  // then they sat in a menu headed "Move 20 selected…" and silently wrote one node. The scope is
  // `nodeActionItems`, the same answer the moves and Delete read; the label says which it got, and
  // `sharedOwnPool` decides whether any chip may render as already-selected — the first node's
  // pool is not the batch's.
  const poolMenu = (
    target: ActionTarget,
    currentPool: string | null | undefined,
  ): React.ReactNode => {
    if (!onSetPool) return null;
    const count = targetNodeCount(target);
    const many = target.kind === 'nodes';
    const choices = poolChoices(pools ?? [], currentPool);
    const inherited = !currentPool?.trim();
    const chip = (
      key: string,
      label: string,
      value: string | null,
      opts: { current?: boolean; warn?: boolean } = {},
    ) => (
      <button
        type="button"
        key={key}
        className={`ntree-dur${opts.current ? ' is-current' : ''}${opts.warn ? ' warn' : ''}`}
        // The warning is spelled out for screen readers and on hover, not carried by colour alone.
        title={opts.warn ? t('tree.poolNoLivePoller') : undefined}
        onClick={() => {
          onSetPool(target, value);
          setMenu(null);
        }}
      >
        {label}
        {opts.warn && <span aria-hidden="true"> !</span>}
      </button>
    );
    return (
      <>
        <div className="ntree-menu-sep" />
        <div className="ntree-menu-section">
          <div className="ntree-menu-label">
            {many ? t('tree.poolSelected', { count }) : t('tree.pool')}
          </div>
          <div className="ntree-menu-durs">
            {choices.map((c) =>
              chip(c.name, c.name, c.name, { current: c.current, warn: !c.live }),
            )}
            {chip('__inherit', t('tree.poolInherit'), '', { current: inherited })}
            {chip('__custom', t('tree.custom'), null)}
          </div>
        </div>
      </>
    );
  };

  /** Outside click and Escape both land here, from `AnchoredPopover` (ADR-124 Inc.2). Stable, so
   *  the popover's document listeners are not re-subscribed on every SSE-driven render. */
  const closeMenu = useCallback(() => setMenu(null), []);

  /** Whether the context menu was open when the pointer went down on the tree body.
   *
   *  🚨 The popover dismisses on **mousedown**, so by the time the body's `click` fires `menu` is
   *  already null — and a body that read it then would close the menu AND clear `?sel=` in one
   *  press, which is the layering ADR-073 決定 4 forbids (transient first). The old menu closed on
   *  `click`, after the body had already seen it open; this ref is that ordering, kept. */
  const menuAtDown = useRef(false);

  /** Where the tree was parked when the pointer last went down on **a control**, or null when
   *  there is nothing to put back (ADR-124 増分 5).
   *
   *  🚨 **Only a press on a control pins, and any scroll clears it.** A pin taken from every press
   *  goes stale — pressing the scrollbar fires no `click` on the body, so nothing resets it, and
   *  the next keyboard focus would drag the operator back to where they were before their own
   *  wheel. A keyboard `Tab` has no pin at all, so the browser still scrolls the target into
   *  view, which for a keyboard is the correct behaviour. */
  const pinned = useRef<ScrollAt | null>(null);

  const reset = () => {
    setDrag(null);
    setDropTarget(null);
  };

  // 🚨 The end of a drag is heard on the DOCUMENT, not only on the row that was grabbed. That row
  // is virtualized: a folder above it delivering its members, or the browser's own edge-scroll,
  // takes it out of the mounted window mid-drag, and a detached node's `dragend` never reaches
  // React. The drag state then outlived the drag — rows left dimmed and a phantom insertion slot
  // wedged into the list until the next drag began. `drop` is here too, for a release over
  // something that is not a drop target of ours at all.
  const dragging = drag !== null;
  useEffect(() => {
    if (!dragging) return undefined;
    const end = () => {
      setDrag(null);
      setDropTarget(null);
    };
    document.addEventListener('dragend', end);
    document.addEventListener('drop', end);
    return () => {
      document.removeEventListener('dragend', end);
      document.removeEventListener('drop', end);
    };
  }, [dragging]);

  /** Every row this drag will move. 🚨 **`.dragging` used to name the grabbed row alone**, so a
   *  three-row selection dimmed one row — which was the defect saying so on screen, a release
   *  before anyone read it (ADR-124 Inc.4). A `Set` rather than `ids.includes` because the batch
   *  runs to 1000 and this is asked once per visible row per render. */
  const draggingIds = useMemo(
    () => new Set(drag === null ? [] : drag.kind === 'node' ? drag.ids : [drag.id]),
    [drag],
  );

  /** What the drag is carrying, for the insertion slot to name (ADR-162 増分 2). Resolved from what
   *  is already in hand rather than captured at `dragstart` — see `dragPreview`. */
  const dragged = useMemo(() => dragPreview(flat, groups, drag), [flat, groups, drag]);

  /** The cursor's position inside the row, as the numbers `dropPosition` decides from. */
  const positionFor = (e: React.DragEvent, targetIsGroup: boolean): DropPos => {
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    return dropPosition(e.clientY - rect.top, rect.height, targetIsGroup, drag);
  };

  const onRowDragOver = (e: React.DragEvent, target: Target, targetIsGroup: boolean) => {
    if (!drag) return;
    e.preventDefault();
    e.stopPropagation();
    e.dataTransfer.dropEffect = 'move';
    const position = positionFor(e, targetIsGroup);
    const ok = dropAllowed(groups, drag, target, position);
    // 🚨 **Only write when the answer changed.** `dragover` fires continuously while the pointer is
    // held still, and a fresh object every time re-rendered the whole tree sixty times a second for
    // nothing. It was merely wasteful before; since ADR-162 増分 2 each of those renders also
    // rebuilds the drawn row list, which is the length of the inventory.
    setDropTarget((prev) =>
      prev &&
      prev.ok === ok &&
      prev.position === position &&
      prev.target !== 'root' &&
      prev.target.id === target.id &&
      prev.target.scope === target.scope
        ? prev
        : { target, position, ok },
    );
  };

  /**
   * A drag held over the insertion slot itself.
   *
   * 🚨 **A slot placed *before* a row sits exactly where the pointer is**, so it is not decoration —
   * it is where the operator actually lets go. That makes it owe two things, and getting either
   * wrong fails quietly (ADR-162 増分 2):
   *
   * 1. **It must accept the drag.** An element is only a drop target while its `dragover` is
   *    cancelled, so without this handler the browser refuses the drop and the gesture dies at the
   *    one place the operator is aiming at. Nothing flickers; nothing errors; the folder stays put.
   * 2. **It must not re-judge.** The slot has no target of its own, so asking again answers
   *    "nothing" — the feedback withdraws, the rows close back up, the pointer is over the original
   *    row again and the slot returns. `dragover` keeps firing while the pointer is held still, so
   *    that is a standing flicker, not a one-off. Verified by doing it: adding `setDropTarget(null)`
   *    here turns the Tier1 slot test red and nothing else.
   */
  const onSlotDragOver = (e: React.DragEvent) => {
    if (!drag) return;
    e.preventDefault();
    e.stopPropagation();
    e.dataTransfer.dropEffect = 'move';
  };

  /** Letting go on the slot performs the placement the slot is drawing — replayed from the recorded
   *  target, because the row under the pointer is the slot and has no target of its own. */
  const onSlotDrop = (e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    const at = dropTarget;
    if (drag && at && at.ok && at.target !== 'root') {
      perform(dropAction(drag, at.target, at.position));
    }
    reset();
  };

  const onRowDrop = (e: React.DragEvent, target: Target, targetIsGroup: boolean) => {
    e.preventDefault();
    e.stopPropagation();
    if (!drag) return;
    // What was SHOWN, not what is under the pointer now — see `dropToPerform`. The row here may
    // have slid under a pointer that never moved.
    const position = positionFor(e, targetIsGroup);
    const at = dropToPerform(dropTarget, {
      target,
      position,
      ok: dropAllowed(groups, drag, target, position),
    });
    if (at) perform(dropAction(drag, at.target, at.position));
    reset();
  };

  /** Turn a decided action into the callback it names. The exhaustive switch is what makes a new
   *  `DropAction` shape a compile error here rather than a drop that does nothing. */
  const perform = (a: DropAction) => {
    switch (a.kind) {
      case 'move-nodes':
        // The placement is passed only when the drop actually named a sibling — a `{}` would put
        // two absent keys on the request body for every plain "move into this folder".
        return onMoveNodes(
          a.nodeIds,
          a.groupId,
          a.before || a.after
            ? a.before
              ? { before: a.before }
              : { after: a.after }
            : undefined,
        );
      case 'move-group':
        return onMoveGroup(a.groupId, a.parentId);
      case 'reorder-group':
        return onReorderGroup(a.groupId, {
          parentId: a.parentId,
          ...(a.before ? { before: a.before } : { after: a.after }),
        });
    }
  };

  const dropOnRoot = () => {
    if (!drag) return;
    perform(rootDropAction(drag));
    reset();
  };

  /** Open or close a folder — the ▶'s press, and Right / Left / Enter's (ADR-155 決定 6). One path,
   *  so a folder closed from the keyboard is saved to the account exactly as the ▶ saves it.
   *
   *  One rule in every mode (ADR-154): the saved layout gets the opposite of what this row shows, and
   *  a press under a search is also recorded so the row can show it. The layout is read from the
   *  store rather than this render's copy, so two presses inside one frame cannot write from the
   *  same stale set. */
  const pressFolder = (id: string, isOpen: boolean) => {
    const next = pressTwisty(
      {
        collapsed: usePrefsStore.getState().nodeTreeCollapsed,
        touched: useTreeTouchedStore.getState().touched,
      },
      { id, isOpen, searching, key: collapseKey },
    );
    setNodeTreeCollapsed(next.collapsed);
    if (next.touched !== touched) useTreeTouchedStore.getState().setTouched(next.touched);
  };

  /**
   * Drop-feedback class for a row that is the current target.
   *
   * ⚠️ **Only `inside` and a refusal mark the target row now** (ADR-162 増分 2). `before`/`after`
   * used to draw a 2px line on the matching edge and no longer draw anything: the placement is shown
   * by the slot row `withDropSlot` inserts, which can say at what depth — and therefore into which
   * folder — the drop lands, where an edge on a row could not.
   */
  const dropClass = (id: string): string => {
    if (!dropTarget || dropTarget.target === 'root' || dropTarget.target.id !== id) return '';
    if (!dropTarget.ok) return ' drop-bad';
    return dropTarget.position === 'inside' ? ' drop-inside' : '';
  };

  /** The folder a permitted drop would write into, so its row can say so as well (ADR-162 増分 2).
   *  Null at the top level and among the Ungrouped nodes, which have no folder row. */
  const parentMarkId = dropParentId(dropTarget);

  /** The branch lines at the start of row `index` (ADR-171): one cell per ancestor level, centred
   *  under that level's twisty. Tones come from `cellTones`; the colours are the stylesheet's. */
  const guideCells = (index: number): React.ReactNode => {
    const cols: GuideKind[] | undefined = guides[index];
    if (!cols || cols.length === 0) return null;
    const litCols = lit.get(index);
    return cols.map((kind, k) => {
      if (!kind) return null;
      const tone = cellTones(litCols?.get(k));
      return (
        <span
          key={k}
          className={`ntree-guide ntree-guide-${kind}`}
          aria-hidden="true"
          data-up={tone.up}
          data-down={tone.down}
          data-stub={tone.stub}
          style={{ left: BASE_PAD + k * INDENT + GUIDE_X }}
        />
      );
    });
  };

  const groupRow = (row: Extract<FlatRow, { kind: 'group' }>, index: number): React.ReactNode => {
    const { group, depth, isOpen, hasChildren, tally } = row;
    const picked = pickCounts.get(group.id) ?? 0;
    const isSel = shown?.kind === 'group' && shown.id === group.id;
    const target: Target = { kind: 'group', id: group.id, scope: group.parent_id ?? null };
    // Null for a folder a person made — and for an origin this build does not know (see the lib).
    const origin = groupOriginOf(group);
    return (
      <div
        id={rowDomId({ kind: 'group', id: group.id })}
        role="treeitem"
        aria-level={ariaLevel(row)}
        aria-selected={isSel}
        aria-expanded={hasChildren ? isOpen : undefined}
        className={`ntree-row ntree-grow${isSel ? ' sel' : ''}${dropClass(group.id)}${parentMarkId === group.id ? ' drop-parent' : ''}${draggingIds.has(group.id) ? ' dragging' : ''}`}
        style={{ paddingLeft: depth * INDENT + BASE_PAD }}
        draggable={canEdit}
        onClick={() => selectGroup(group)}
        onDragStart={(e) => {
          e.stopPropagation();
          e.dataTransfer.effectAllowed = 'move';
          setDrag({ kind: 'group', id: group.id });
        }}
        onDragEnd={reset}
        onDragOver={(e) => onRowDragOver(e, target, true)}
        onDrop={(e) => onRowDrop(e, target, true)}
        onContextMenu={(e) => {
          if (!groupMenuHasItems(caps)) return;
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY, kind: 'group', group });
        }}
      >
        {guideCells(index)}
        {/* Out of the Tab order, like the name beside it (ADR-155 決定 1): the tree is one Tab stop,
            and Right / Left / Enter open and close from the keyboard. */}
        <button
          type="button"
          tabIndex={-1}
          className={`ntree-twisty${isOpen ? ' open' : ''}`}
          onClick={(e) => {
            e.stopPropagation();
            pressFolder(group.id, isOpen);
          }}
          aria-label={isOpen ? t('nav:shell.collapse') : t('nav:shell.expand')}
          disabled={!hasChildren}
        >
          ▶
        </button>
        <span className="ntree-icon">
          <GroupIcon type={asGroupType(group.group_type)} />
        </span>
        <button
          type="button"
          tabIndex={-1}
          className="ntree-grp-name"
          onClick={(e) => {
            e.stopPropagation();
            selectGroup(group);
          }}
        >
          {group.name}
        </button>
        {/* How many of the working set are inside, closed or not (ADR-171 決定 4). Its own text is
            the fact; the title only spells it out (ADR-055 R4). */}
        {picked > 0 && (
          <span className="ntree-pick" title={t('tree.pickCountTitle', { count: picked })}>
            {t('tree.pickCount', { count: picked })}
          </span>
        )}
        {/* An integration made this folder and still keeps it (ADR-164 Inc.7): the organization's
            tree goes when the organization does, and a NetBox sync renames and re-parents its
            folders over whatever was typed. The badge's own text is the fact — the `title` only
            elaborates, so nothing here is hover-only (ADR-055 R4). `.ntree-badge` does not shrink,
            so it is the name that gives way in the row, never the mark. `role="img"` as on the pin
            mark below: an `aria-label` on a span with no role is not reliably read out. */}
        {origin && (
          <span
            className={`ntree-badge${brandBadgeClass(GROUP_ORIGIN_BADGE_BRANDS[origin])}`}
            role="img"
            title={t(`tree.origin.${origin}`)}
            aria-label={t(`tree.origin.${origin}`)}
          >
            {GROUP_ORIGIN_BADGES[origin]}
          </span>
        )}
        {/* A mark, not a control: pinning is in the row's menu and the detail pane. */}
        {pins?.groups.has(group.id) && (
          <span className="ntree-pin" role="img" title={t('tree.pinnedMark')} aria-label={t('tree.pinnedMark')}>
            <PinIcon />
          </span>
        )}
        {/* `tally === null` is "the rollup has not answered yet" (ADR-133), and the two elements
            still occupy their width. Dropping them instead would let the name column stretch and
            then snap back as each answer lands — the tree moving for a reason that is not the
            operator, which ADR-124 増分 5 exists to stop. An empty pill reads as a skeleton; a `0`
            would read as an empty folder. */}
        {tally ? (
          <HealthBar tally={tally} className="ntree-health" />
        ) : (
          <span className="ntree-health" />
        )}
        <span className="ntree-count">{tally ? tally.total : ''}</span>
        {/* The hover-revealed actions come BEFORE the markers on purpose — see `.ntree-actions` in
            the stylesheet. Revealing them shifts everything to their left, and a marker that moves
            under the pointer is a mis-click onto Delete group. */}
        {canEdit && (
          // `menu-open` keeps the hover-revealed actions rendered while this row's ＋ menu is up:
          // the menu opens BELOW the row, so reaching it takes the pointer off the row, and the
          // hover rule would otherwise unmount the menu on the way there.
          <span
            className={`ntree-actions${addMenuGroup === group.id ? ' menu-open' : ''}`}
          >
            <ActionMenu
              label={t('addMenu.label')}
              align="end"
              onOpenChange={(o) => setAddMenuGroup(o ? group.id : null)}
              items={[
                ...(onAddNode
                  ? [
                      {
                        key: 'node',
                        label: t('tree.addNodeHere'),
                        onSelect: () => onAddNode(group.id),
                      },
                    ]
                  : []),
                {
                  key: 'group',
                  label: t('group.addSubgroup'),
                  onSelect: () => onAddGroup(group.id),
                },
              ]}
              trigger={(p) => (
                <button
                  {...p}
                  type="button"
                  className="ntree-act"
                  title={t('addMenu.trigger')}
                  aria-label={t('addMenu.trigger')}
                >
                  ＋
                </button>
              )}
            />
            <button
              type="button"
              className="ntree-act"
              title={t('tree.editMoveGroup')}
              onClick={(e) => {
                e.stopPropagation();
                onEditGroup(group);
              }}
            >
              ✎
            </button>
            <button
              type="button"
              className="ntree-act"
              title={t('group.delete')}
              onClick={(e) => {
                e.stopPropagation();
                onDeleteGroup(group);
              }}
            >
              🗑
            </button>
          </span>
        )}
        {suppressionMarks({
          target: { kind: 'group', id: group.id, name: group.name },
          maint: !!suppression?.maintenanceGroups.has(group.id),
          muted: !!suppression?.muteGroups.has(group.id),
        })}
      </div>
    );
  };

  const renderNode = (
    node: NodeSummary,
    depth: number,
    level: number,
    index: number,
  ): React.ReactNode => {
    const target: Target = { kind: 'node', id: node.id, scope: node.group_id ?? null };
    const isSel = shown?.kind === 'node' && shown.id === node.id;
    // 🚨 A class of its own, never `sel`. `tests/ui/treeDeselect.spec.ts` pins `.ntree-row.sel`
    // at exactly one row, which is the property that proves the pane's selection is single.
    const isChecked = checkedNodes.has(node.id);
    const move = nodeMoveItems(checkedNodes, node.id, canEdit);
    return (
      <div
        id={rowDomId({ kind: 'node', id: node.id })}
        role="treeitem"
        aria-level={level}
        aria-selected={isSel}
        className={`ntree-row ntree-node${isSel ? ' sel' : ''}${isChecked ? ' checked' : ''}${dropClass(node.id)}${draggingIds.has(node.id) ? ' dragging' : ''}`}
        key={node.id}
        style={{ paddingLeft: depth * INDENT + BASE_PAD }}
        draggable={canEdit}
        onClick={(e) => clickNode(e, node)}
        onDragStart={(e) => {
          e.stopPropagation();
          e.dataTransfer.effectAllowed = 'move';
          // What this carries is `nodeTreeDnd.ts`'s call, not a shape spelled out here — the
          // same rule the ↗ two elements below reads through `nodeMoveItems`.
          setDrag(nodeDragItem(checkedNodes, node.id));
        }}
        onDragEnd={reset}
        onDragOver={(e) => onRowDragOver(e, target, false)}
        onDrop={(e) => onRowDrop(e, target, false)}
        onContextMenu={(e) => {
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY, kind: 'node', node });
        }}
      >
        {guideCells(index)}
        {/* Spacer keeps the status dot in the same column as a group's icon at this depth. */}
        <span className="ntree-twisty ntree-twisty-spacer" aria-hidden="true" />
        <span className="ntree-icon">
          <StatusDot state={node.state} withLabel={false} />
        </span>
        <button
          type="button"
          tabIndex={-1}
          className="ntree-node-name"
          // Every row: the name can be cut off, and the address is how two rows are told apart.
          title={`${node.name} — ${node.address}`}
          onClick={(e) => {
            e.stopPropagation();
            clickNode(e, node);
          }}
        >
          {node.name}
        </button>
        {sameName.has(node.id) && <span className="ntree-node-addr">{node.address}</span>}
        {pins?.nodes.has(node.id) && (
          <span className="ntree-pin" role="img" title={t('tree.pinnedMark')} aria-label={t('tree.pinnedMark')}>
            <PinIcon />
          </span>
        )}
        {/* What kind of node this is, when it is not an ordinary ICMP/SNMP device — a URL monitor,
            a DNS monitor or a Meraki device. Unmarked is the default: the tree is overwhelmingly
            ordinary devices, so a badge on every one of 50k rows would say nothing. */}
        {nodeBadges({ kind: node.kind, merakiProductType: node.meraki_product_type }).map(
          (badge) => (
            <NodeBadgeTag
              key={badge.text}
              badge={badge}
              className="ntree-badge"
              label={t(badge.labelKey)}
            />
          ),
        )}
        {/* Before the markers — see `.ntree-actions` in the stylesheet. The ↗ acts on whatever the
            menu's move items act on: this row, or the working set it belongs to (`nodeMoveItems`,
            ADR-124 Inc.2) — one rule, not a second copy of it. */}
        {move && (
          <span className="ntree-actions">
            <button
              type="button"
              className="ntree-act"
              title={
                move.scope === 'selection' && onMoveChecked
                  ? t('tree.moveSelected', { count: move.count })
                  : t('tree.moveToGroup')
              }
              onClick={(e) => {
                e.stopPropagation();
                if (move.scope === 'selection' && onMoveChecked) onMoveChecked();
                else onRequestMoveNode(node);
              }}
            >
              ↗
            </button>
          </span>
        )}
        {suppressionMarks({
          target: { kind: 'node', id: node.id, name: node.name },
          node,
          // The index already accounts for a release, including re-adding a window that names the
          // node. `state` is the engine's rolled-up opinion and lags a release by up to one refresh
          // (~30s), so it is only consulted while the row is *not* released — otherwise the wrench
          // would sit next to the struck-through one for half a minute after every release.
          maint:
            !!suppression?.maintenanceNodes.has(node.id) ||
            (node.state === 'maintenance' && !suppression?.exemptMaintenanceNodes.has(node.id)),
          muted: !!suppression?.muteNodes.has(node.id),
          releasedMaint: !!suppression?.exemptMaintenanceNodes.has(node.id),
          releasedMute: !!suppression?.exemptMuteNodes.has(node.id),
        })}
      </div>
    );
  };

  // The Ungrouped section header row — also the root drop zone (drop here → move to top level) and
  // the right-click "add at top level" target. In the flattened list it's a single row; the old
  // wrapper's dashed separator moves onto the row via `.ntree-ungrouped-head`.
  const ungroupedHeadRow = (count: number): React.ReactNode => {
    const rootDropActive = dropTarget?.target === 'root' && !!drag;
    return (
      <div
        role="none"
        className={`ntree-row ntree-ungrouped-head${rootDropActive ? ' drop-inside' : ''}`}
        style={{ paddingLeft: BASE_PAD }}
        onDragOver={(e) => {
          if (!drag) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = 'move';
          // Only when the answer changed — `dragover` repeats while the pointer is held still, and
          // each write rebuilds the drawn row list. The rows learnt this in ADR-162 増分 2; this
          // handler was the one that did not.
          setDropTarget((prev) =>
            prev && prev.target === 'root' ? prev : { target: 'root', position: 'inside', ok: true },
          );
        }}
        onDrop={(e) => {
          e.preventDefault();
          dropOnRoot();
        }}
        onContextMenu={(e) => {
          if (!rootMenuHasItems(caps)) return;
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY, kind: 'root' });
        }}
      >
        <span className="ntree-twisty ntree-twisty-spacer" aria-hidden="true" />
        <span className="ntree-icon ntree-ungrouped-icon">⌁</span>
        <span className="ntree-grp-name ntree-ungrouped-label">{t('ungrouped')}</span>
        <span className="ntree-count">{count}</span>
      </div>
    );
  };

  // Placeholder shown under an open group whose members are still being lazily fetched (A-3).
  // 🚨 Both placeholders accept a drop, as "into the folder they stand in for". They are drawn
  // indented inside it and read as its interior — but they had no `dragover`, so the browser
  // refused the drop: a node released on "Loading nodes…" went nowhere, with the folder above
  // still outlined as the destination and nothing said.
  const placeholderDrop = (groupId: string) => {
    const group = groups.find((g) => g.id === groupId);
    const target: Target = { kind: 'group', id: groupId, scope: group?.parent_id ?? null };
    return {
      onDragOver: (e: React.DragEvent) => {
        if (!drag) return;
        e.preventDefault();
        e.stopPropagation();
        e.dataTransfer.dropEffect = 'move';
        const ok = dropAllowed(groups, drag, target, 'inside');
        setDropTarget((prev) =>
          prev &&
          prev.ok === ok &&
          prev.position === 'inside' &&
          prev.target !== 'root' &&
          prev.target.id === groupId
            ? prev
            : { target, position: 'inside', ok },
        );
      },
      onDrop: (e: React.DragEvent) => {
        e.preventDefault();
        e.stopPropagation();
        if (drag && dropAllowed(groups, drag, target, 'inside')) {
          perform(dropAction(drag, target, 'inside'));
        }
        reset();
      },
    };
  };

  const loadingRow = (depth: number, groupId: string, index: number): React.ReactNode => (
    <div
      className="ntree-row ntree-loading"
      role="none"
      style={{ paddingLeft: depth * INDENT + BASE_PAD }}
      {...placeholderDrop(groupId)}
    >
      {guideCells(index)}
      <span className="ntree-twisty ntree-twisty-spacer" aria-hidden="true" />
      <span className="ntree-loading-label muted">{t('tree.loadingNodes')}</span>
    </div>
  );

  // A group whose members could not be fetched (ADR-125). Says so, and offers the retry — because
  // nothing retries on its own any more, and a row that only said "loading" would be a lie the
  // operator waits on forever (ADR-055 R6: say it where they are looking).
  const failedRow = (depth: number, groupId: string, index: number): React.ReactNode => (
    <div
      className="ntree-row ntree-failed"
      role="none"
      style={{ paddingLeft: depth * INDENT + BASE_PAD }}
      {...placeholderDrop(groupId)}
    >
      {guideCells(index)}
      <span className="ntree-twisty ntree-twisty-spacer" aria-hidden="true" />
      <span className="ntree-failed-label">{t('tree.loadFailed')}</span>
      {onRetryGroup && (
        <button type="button" className="ntree-retry" onClick={() => onRetryGroup(groupId)}>
          {t('tree.retry')}
        </button>
      )}
    </div>
  );

  /**
   * The insertion slot: the row the drop would create, drawn where it would land (ADR-162 増分 2).
   *
   * 🚨 **The indentation is the message.** A folder dropped at the bottom edge of a folder's last
   * node lands *inside* that folder; five pixels lower it lands beside it, one level up. The 2px
   * line this replaced drew the same mark in both cases and could say nothing about which — the
   * operator's report was exactly that question. Here the name sits in the column it will occupy.
   *
   * ⚠️ `role="none"` and out of the keyboard's reach (`rowSelection` answers null for this kind), so
   * the slot cannot become a cursor position or a selection. It is feedback, not inventory.
   */
  const slotRow = (depth: number, index: number): React.ReactNode => (
    <div
      className="ntree-row ntree-drop-slot"
      role="none"
      style={{ paddingLeft: depth * INDENT + BASE_PAD }}
      // The slot sits under the pointer whenever the placement is "before" something, so it is where
      // the operator lets go — it has to accept the drop and perform it. See `onSlotDragOver`.
      onDragOver={onSlotDragOver}
      onDrop={onSlotDrop}
      title={t('tree.dropHere')}
    >
      {guideCells(index)}
      <span className="ntree-twisty ntree-twisty-spacer" aria-hidden="true" />
      <span className="ntree-icon">
        {dragged?.kind === 'group' ? (
          <GroupIcon type={asGroupType(dragged.group.group_type)} />
        ) : (
          <span className="ntree-slot-dot" aria-hidden="true" />
        )}
      </span>
      <span className="ntree-slot-name">
        {dragged === null
          ? t('tree.dropHere')
          : dragged.kind === 'group'
            ? dragged.group.name
            : dragged.name}
      </span>
      {dragged?.kind === 'node' && dragged.extra > 0 && (
        <span className="ntree-slot-more">{t('tree.dropMore', { count: dragged.extra })}</span>
      )}
    </div>
  );

  /**
   * One pinned folder in the band (ADR-171 決定 5). A lookalike, not a tree row:
   *
   * 🚨 **Never `.ntree-row`, never an id, never `role="treeitem"`.** `treeDeselect.spec.ts` counts
   * `.ntree-row.sel` to prove the selection is single, `aria-activedescendant` names a row by its
   * DOM id, and a screen reader would read every pinned folder twice. The band is `aria-hidden`;
   * the keyboard already reaches a parent with ←.
   *
   * A click scrolls back to the folder's own row and selects it — never deselects it, which is what
   * a second click on the real row does (ADR-073): the band is a way back, not a toggle.
   */
  const stickyRow = (index: number): React.ReactNode => {
    const row = drawn[index];
    if (row.kind !== 'group' && row.kind !== 'ungrouped-head') return null;
    const depth = row.kind === 'group' ? row.depth : 0;
    return (
      <div
        key={flatRowKey(row)}
        className="ntree-sticky-row"
        style={{ paddingLeft: depth * INDENT + BASE_PAD }}
        // Keep focus where it is: a mousedown here would otherwise move it onto the tree body and
        // run the scroll pin (`pinFocusScroll`) against a scroll this click is about to make.
        onMouseDown={(e) => e.preventDefault()}
        onClick={(e) => {
          e.stopPropagation();
          rowVirtualizer.scrollToIndex(index, { align: 'start' });
          if (row.kind !== 'group') return;
          if (isSelected('group', row.group.id) && !cursor) return;
          setCursor(null);
          if (!onSelectGroup) return;
          showClicked({ kind: 'group', id: row.group.id });
          onSelectGroup(row.group);
        }}
      >
        <span className="ntree-twisty ntree-twisty-spacer open">▶</span>
        {row.kind === 'group' ? (
          <>
            <span className="ntree-icon">
              <GroupIcon type={asGroupType(row.group.group_type)} />
            </span>
            <span className="ntree-sticky-name" title={row.group.name}>
              {row.group.name}
            </span>
            <span className="ntree-count">{row.tally ? row.tally.total : ''}</span>
          </>
        ) : (
          <>
            <span className="ntree-icon ntree-ungrouped-icon">⌁</span>
            <span className="ntree-sticky-name">{t('ungrouped')}</span>
            <span className="ntree-count">{row.count}</span>
          </>
        )}
      </div>
    );
  };

  const renderRow = (row: FlatRow, index: number): React.ReactNode => {
    switch (row.kind) {
      case 'drop-slot':
        return slotRow(row.depth, index);
      case 'group':
        return groupRow(row, index);
      case 'node':
      case 'ungrouped-node':
        return renderNode(row.node, row.depth, ariaLevel(row), index);
      case 'group-loading':
        return loadingRow(row.depth, row.groupId, index);
      case 'group-failed':
        return failedRow(row.depth, row.groupId, index);
      case 'ungrouped-head':
        return ungroupedHeadRow(row.count);
    }
    // 🚨 **Exhaustiveness has to be asked for, and this switch did not ask.** The return type is
    // `React.ReactNode`, which includes `undefined`, so a switch that falls through compiles
    // cleanly and renders nothing — a new `FlatRow` variant would ship as an invisible row with
    // every check green. (`tsconfig.json` has `strict` and `noFallthroughCasesInSwitch` but not
    // `noImplicitReturns`, and neither of those two sees this.) Assigning the narrowed `row` to
    // `never` is what makes the compiler demand a decision here. Found while adding
    // `group-failed` (ADR-125), whose whole reason for being a variant was this guarantee.
    const unhandled: never = row;
    return unhandled;
  };

  const virtualRows = rowVirtualizer.getVirtualItems();

  /** Publish the on-screen folders that are still waiting for members, so the fetch follows the
   *  viewport the way the rendering already does (ADR-125).
   *
   *  🚨 **No judgement lives here.** Which rows count is `pendingGroupKeys` in `lib/nodeTree.ts`,
   *  where Vitest can reach it. ADR-124 Inc.2 paid for the other arrangement: the one branch left
   *  in a `.tsx` was the one branch no test could run, and it was the one that was wrong.
   *
   *  🚨 **The settle is not a nicety — it is what stops a momentum scroll from re-creating the
   *  burst.** Flicking through 500 folders makes every one of them briefly "on screen and waiting";
   *  without a settle each is queued, and the queue bounds the RATE, not the total — so all 501
   *  would still be fetched, six at a time. The first publish is immediate (`ms = 0`), because
   *  delaying the initial screenful buys nothing.
   *
   *  Joined into a string so the debounce compares CONTENT: `getVirtualItems()` returns a fresh
   *  array every render and would otherwise re-arm the timer forever. Group ids are UUIDs, so a
   *  comma cannot appear inside one. */
  const publishedOnce = useRef(false);
  const pendingKey = pendingGroupKeys(
    // An index can briefly fall outside `drawn` between a virtualizer measure and a re-render.
    virtualRows.map((v) => drawn[v.index]).filter((r): r is FlatRow => r !== undefined),
  ).join(',');
  const settledKey = useDebouncedValue(pendingKey, publishedOnce.current ? PENDING_SETTLE_MS : 0);
  useEffect(() => {
    if (!onPendingGroupsChange) return;
    publishedOnce.current = true;
    onPendingGroupsChange(settledKey ? settledKey.split(',') : []);
  }, [settledKey, onPendingGroupsChange]);

  // ---- The keyboard (ADR-155). Every decision is `nodeTreeKeys.ts`'s; this block applies them. ----

  const settledCursor = useDebouncedValue(cursor, CURSOR_SETTLE_MS);
  // `selected` and the page's callbacks are new objects on every render, so this runs on most renders
  // anyway; `settleCursor` holds the identity checks that make a re-run a no-op.
  useEffect(() => {
    const index = indexOfSelection(drawn, settledCursor);
    switch (settleCursor(settledCursor, cursor, selected ?? null, committed.current, index >= 0)) {
      case 'wait':
        return;
      case 'clear':
        setCursor(null);
        return;
      case 'commit': {
        committed.current = settledCursor;
        const row = drawn[index];
        if (row.kind === 'group') {
          if (onSelectGroup) onSelectGroup(row.group);
          else setCursor(null);
          return;
        }
        const node = rowNode(row);
        if (!node || !onSelectNode) {
          setCursor(null);
          return;
        }
        // The anchor catches up once, here, rather than on every repeat (`moveCheckedChange`).
        const anchor = anchorOnSettle(checkedNodes.size, anchorId ?? null, node.id);
        if (anchor !== null && onCheckedChange) onCheckedChange(new Map(), anchor);
        onSelectNode(node);
      }
    }
  }, [
    drawn,
    settledCursor,
    cursor,
    selected,
    onSelectNode,
    onSelectGroup,
    checkedNodes.size,
    anchorId,
    onCheckedChange,
  ]);

  /** `selected` is rebuilt from the URL on every render, so the effect below is keyed on this. */
  const selectedKey = selected ? rowDomId(selected) : '';
  useEffect(() => {
    const wrote = committed.current;
    committed.current = null;
    setCursor((c) => cursorAfterSelection(c, selected ?? null, wrote));
    // eslint-disable-next-line react-hooks/exhaustive-deps -- keyed on the selection's identity; `selected` itself is a new object every render
  }, [selectedKey]);

  /** Put the cursor on a row: the working set first (it is page state, and not debounced), then the
   *  cursor, then the scroll — the one scroll this tree writes (ADR-155 決定 4). */
  const moveCursor = (index: number, gesture: MoveGesture) => {
    const row = drawn[index];
    const sel = rowSelection(row);
    if (!sel) return;
    const node = rowNode(row);
    if (node && onCheckedChange) {
      const change = moveCheckedChange(gesture, node, {
        flat: drawn,
        anchorId: anchorId ?? null,
        selection: shown,
        checked: checkedNodes,
      });
      if (change) onCheckedChange(change.checked, change.anchorId);
    }
    setCursor(cursorForMove(sel, selected ?? null, committed.current));
    rowVirtualizer.scrollToIndex(index, { align: 'auto' });
  };

  /** Set by a menu opened from the keyboard, read when the menu has been placed (focus the first
   *  item) and when it closes (give focus back to the tree). A pointer open sets neither. */
  const menuOpenedByKey = useRef(false);
  const focusMenuOnPlace = useRef(false);
  /** `AnchoredPopover` reports being placed, but not being unmounted — so this is reset when the
   *  menu closes, or the next keyboard open would focus an item that is still hidden. */
  const [menuPlaced, setMenuPlaced] = useState(false);

  const menuItems = (root: ParentNode | null): HTMLButtonElement[] =>
    root ? [...root.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')] : [];

  const openMenuFromKey = (index: number) => {
    const row = drawn[index];
    const sel = rowSelection(row);
    if (!sel) return;
    rowVirtualizer.scrollToIndex(index, { align: 'auto' });
    const open = () => {
      const box = (
        document.getElementById(rowDomId(sel)) ?? scrollRef.current
      )?.getBoundingClientRect();
      if (!box) return;
      const x = box.left + MENU_KEY_INSET_PX;
      const y = box.bottom;
      if (row.kind === 'group') {
        if (!groupMenuHasItems(caps)) return;
        setMenu({ x, y, kind: 'group', group: row.group });
      } else {
        const node = rowNode(row);
        if (!node) return;
        setMenu({ x, y, kind: 'node', node });
      }
      menuOpenedByKey.current = true;
      focusMenuOnPlace.current = true;
    };
    // 🚨 Read the row's box once the row EXISTS. `scrollToIndex` only writes `scrollTop`; the
    // virtualizer renders on the scroll event that follows, so a cursor row outside the mounted
    // window is not in the DOM on the next line. Reading at once fell through to the scroll
    // container's box, and the menu — which acts on that row — opened at the corner of the pane
    // while the row scrolled into view somewhere else. Two frames: one for the scroll event, one
    // for the render it causes.
    if (document.getElementById(rowDomId(sel))) open();
    else requestAnimationFrame(() => requestAnimationFrame(open));
  };

  // A hidden element cannot take focus, and the popover is hidden until measured — hence placed.
  useLayoutEffect(() => {
    if (!menu || !menuPlaced || !focusMenuOnPlace.current) return;
    focusMenuOnPlace.current = false;
    menuItems(document.querySelector('.ntree-menu'))[0]?.focus({ preventScroll: true });
  }, [menu, menuPlaced]);

  useEffect(() => {
    if (menu) return;
    setMenuPlaced(false);
    focusMenuOnPlace.current = false;
    if (!menuOpenedByKey.current) return;
    menuOpenedByKey.current = false;
    const active = document.activeElement;
    const onDocument = active === null || active === document.body;
    if (shouldRefocusTree(onDocument, document.querySelector('[role="dialog"]') !== null)) {
      scrollRef.current?.focus({ preventScroll: true });
    }
  }, [menu]);

  /** Up / Down / Home / End inside the tree's own menu. Tab closes it and gives focus back to the
   *  tree — the item holding focus goes with the menu, so letting Tab move on from it would start
   *  from nowhere. */
  const onMenuKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'Tab') {
      e.preventDefault();
      setMenu(null);
      return;
    }
    const items = menuItems(e.currentTarget);
    const next = menuStep(e.key, items.indexOf(document.activeElement as HTMLButtonElement), items.length);
    if (next === null) return;
    e.preventDefault();
    items[next].focus({ preventScroll: true });
  };

  const onTreeKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    // A menu open over the tree owns the keyboard; the tree moving under it would leave the menu
    // naming a row that is no longer the current one.
    if (e.defaultPrevented || menu) return;
    const body = e.currentTarget;
    const target = e.target as HTMLElement;
    if (!keyBelongsToTree(target, body, body.contains(target))) return;
    const outcome = treeKeyAction(e, {
      flat: drawn,
      cursor: indexOfSelection(drawn, shown),
      page: pageRows(body.clientHeight, ROW_H),
    });
    if (!outcome) return;
    e.preventDefault();
    // After a click, focus sits on the row's name button, where a claimed Space or Enter would
    // still activate the button when the key comes up. On the body it activates nothing.
    if (document.activeElement !== body) body.focus({ preventScroll: true });
    switch (outcome.kind) {
      case 'none':
        return;
      case 'move':
        return moveCursor(outcome.index, outcome.gesture);
      case 'set-open': {
        const row = drawn[outcome.index];
        if (row.kind === 'group') pressFolder(row.group.id, row.isOpen);
        return;
      }
      case 'check': {
        const node = rowNode(drawn[outcome.index]);
        if (!node || !onCheckedChange) return;
        const change = spaceCheckedChange(node, checkedNodes);
        onCheckedChange(change.checked, change.anchorId);
        return;
      }
      case 'open-node': {
        const node = rowNode(drawn[outcome.index]);
        if (node) onOpenNode(node);
        return;
      }
      case 'menu':
        return openMenuFromKey(outcome.index);
    }
  };

  /** `aria-activedescendant` only while the current row is rendered: a reference to an id that is not
   *  in the document (a row scrolled out of the virtualized window) is an error, not a hint. */
  const activeIndex = indexOfSelection(drawn, shown);
  const activeDescendant =
    shown && virtualRows.some((v) => v.index === activeIndex) ? rowDomId(shown) : undefined;

  /** What the open node menu's move items act on (ADR-124 Inc.2). Decided in `nodeTreeMenu.ts`;
   *  here it is only applied. */
  const moveItems =
    menu?.kind === 'node' ? nodeMoveItems(checkedNodes, menu.node.id, canEdit) : null;

  /** What the open node menu's Delete acts on (ADR-124 増分 6), decided in `nodeTreeMenu.ts`. The
   *  permission is the page's: the item exists only when it wired a delete. */
  const deleteItems =
    menu?.kind === 'node' ? nodeDeleteItems(checkedNodes, menu.node.id, !!onDeleteNode) : null;

  /** Whether the open node menu is one where a *batch* is in play — either because this row
   *  carries the set, or because the set exists on other rows. The row-scoped items name their
   *  node in both cases: the danger is a menu that offers "Move 20 selected…" and "Edit node…"
   *  together, where the second silently means one (ADR-124 増分 9).
   *
   *  ⚠️ Asked through `nodeActionItems` with `true`, not by reading `checkedNodes.size` here — the
   *  question "does this row's gesture involve the batch" has one answer in this codebase, and a
   *  second copy of it is exactly what Inc.4 had to undo in the drag path. */
  const actsOnBatch =
    menu?.kind === 'node'
      ? (() => {
          const items = nodeActionItems(checkedNodes, menu.node.id, true);
          return items?.scope === 'selection' || !!items?.nameTheRow;
        })()
      : false;

  /** The working set as an action target, when the open node menu's row carries it — otherwise
   *  `null` and the item acts on the row. Every batch-aware section reads this one value rather
   *  than re-deciding, which is the rule Inc.4 had to restore in the drag path (ADR-124 増分 10). */
  /** What the open node menu's Poll now acts on, and whether to name the row — the same answer
   *  the moves and Delete read. */
  const pollItems =
    menu?.kind === 'node' ? nodeActionItems(checkedNodes, menu.node.id, !!onPollNodes) : null;

  const batchTarget: ActionTarget | null =
    menu?.kind === 'node' &&
    nodeActionItems(checkedNodes, menu.node.id, true)?.scope === 'selection'
      ? { kind: 'nodes', nodes: [...checkedNodes.values()] }
      : null;

  /** The two items that act on the working set. Rendered in the single item's place when the
   *  right-clicked row is in the set, and below a separator when it is not. */
  const selectionMoveItems = (count: number): React.ReactNode => (
    <>
      {onMoveChecked && (
        <button
          type="button"
          onClick={() => {
            onMoveChecked();
            setMenu(null);
          }}
        >
          {t('tree.moveSelected', { count })}
        </button>
      )}
      {onMoveCheckedByPrefix && canMoveByPrefix(groups, canEdit) && (
        <button
          type="button"
          onClick={() => {
            onMoveCheckedByPrefix();
            setMenu(null);
          }}
        >
          {t('tree.moveSelectedByPrefix', { count })}
        </button>
      )}
      {/* Gated on its own prop, like its two neighbours — never on the menu as a whole. A mixed
          menu closed on its strictest member is how every operator lost the right-click menu once
          (ui-conventions). */}
      {onTagChecked && (
        <button
          type="button"
          onClick={() => {
            onTagChecked();
            setMenu(null);
          }}
        >
          {t('tree.tagSelected', { count })}
        </button>
      )}
    </>
  );

  return (
    <div className="ntree">
      {showToolbar && canEdit && (
        <div className="ntree-toolbar">
          <Button variant="outline" onClick={() => onAddGroup(null)}>
            ＋ {t('group.add')}
          </Button>
          <span className="muted ntree-hint">{t('tree.dragHint')}</span>
        </div>
      )}

      {/* Clicking the empty space below the rows clears the selection (ADR-073 decision 5).
          `e.target === e.currentTarget` is what makes this "blank space" and not "the tree": rows
          live inside the virtualizer's spacer, so a click on one never reports the body as its
          target. Deliberately a React `onClick` and not a `document` listener — the same-frame
          bug `rowMenu.spec.ts` pins (the click that opens a menu also reaching the handler that
          closes it) is specific to document-level dismissal, and this shape cannot have it.
          While the context menu is open the click only dismisses that: transient first (decision 4)
          — decided from what the body saw at mousedown, see `menuAtDown`. */}
      <div
        className="ntree-body"
        ref={scrollRef}
        // One Tab stop for the whole tree, and the rows named through `aria-activedescendant`
        // (ADR-155 決定 1) — a virtualized row that held focus would take it away when scrolled out.
        tabIndex={0}
        role="tree"
        aria-label={t('inventory.treeLabel')}
        aria-activedescendant={activeDescendant}
        onKeyDown={onTreeKeyDown}
        // ADR-124 増分 5 決定 B, both halves, from one place. Capture phase, so a descendant that
        // ever stops mousedown propagation cannot disarm either of them — and `menuAtDown` is the
        // ADR-073 決定 4 ordering, which must not become optional. Same element, same event.
        onMouseDownCapture={(e) => {
          menuAtDown.current = menu !== null;
          pinFocusScroll(scrollRef.current, e.target as Element, (at) => {
            pinned.current = at;
          });
        }}
        // A scroll the OPERATOR made is the one scroll that must survive, so it voids the pin.
        // Our own restore writes fire this asynchronously, after the pin is already null.
        onScroll={() => {
          pinned.current = null;
        }}
        // React's spelling of `focusin`, so a focus landing on any descendant reaches this
        // element — which is what covers every control in every row from a single handler.
        onFocus={(e) => {
          restoreScroll(e.currentTarget, pinned.current);
          pinned.current = null;
        }}
        onClick={(e) => {
          const wasOpen = menuAtDown.current;
          menuAtDown.current = false;
          pinned.current = null;
          if (wasOpen || !onSelectNone) return;
          if (e.target === e.currentTarget) {
            setCursor(null);
            onSelectNone();
          }
        }}
      >
        {drawn.length === 0 ? (
          // Empty flat list: a blank body while filtering with no matches, else loading / empty-state.
          // Pinned only with nothing pinned says how to pin instead (ADR-146, ADR-055 R6) — a blank
          // pane after pressing a button reads as a broken button.
          pinnedFilter && nothingPinned(pinnedFilter) ? (
            <p className="muted ntree-empty">{t('tree.pinnedEmpty')}</p>
          ) : withNodesOnly && groups.length > 0 && !filtering && !loading ? (
            // Folders exist; the switch is what is hiding them. Say so (ADR-055 R6).
            <p className="muted ntree-empty">{t('tree.withNodesEmpty')}</p>
          ) : filtering ? null : loading ? (
            <p className="muted ntree-empty">{t('tree.loadingNodes')}</p>
          ) : (
            <p
              className="muted ntree-empty"
              onContextMenu={(e) => {
                if (!rootMenuHasItems(caps)) return;
                e.preventDefault();
                setMenu({ x: e.clientX, y: e.clientY, kind: 'root' });
              }}
            >
              {t('tree.emptyInventory')}
            </p>
          )
        ) : (
          // Virtualized body: only the on-screen window of `drawn` is turned into DOM (S13).
          <>
            {/* The pinned folders (ADR-171 決定 5). A zero-height sticky element, so it takes no
                room in the scroll height and the rows below keep their `index × 30px` positions. */}
            {band.length > 0 && (
              <div className="ntree-sticky" aria-hidden="true">
                <div className="ntree-sticky-band">{band.map(stickyRow)}</div>
              </div>
            )}
            <div style={{ height: rowVirtualizer.getTotalSize(), position: 'relative' }}>
              {virtualRows.map((vi) => (
                <div
                  key={vi.key}
                  data-index={vi.index}
                  style={{
                    position: 'absolute',
                    top: 0,
                    left: 0,
                    width: '100%',
                    transform: `translateY(${vi.start}px)`,
                  }}
                >
                  {renderRow(drawn[vi.index], vi.index)}
                </div>
              ))}
            </div>
          </>
        )}
      </div>

      {/* The context menu — `AnchoredPopover` at the right-click's point (ADR-124 Inc.2): portalled,
          clamped and flipped to stay inside the viewport, scrolling when taller than it, closed by
          Escape or an outside mousedown. Mounted only while open, so the same element carries every
          variant and a switch between them at the same point re-places rather than re-opens. */}
      {menu && (
        <AnchoredPopover
          open
          at={{ x: menu.x, y: menu.y }}
          role={menu.kind === 'suppress' ? 'dialog' : 'menu'}
          label={
            menu.kind === 'root'
              ? t('tree.menuRoot')
              : t('tree.menuFor', {
                  name:
                    menu.kind === 'group'
                      ? menu.group.name
                      : menu.kind === 'node'
                        ? menu.node.name
                        : menu.target.name,
                })
          }
          className="ntree-menu"
          onDismiss={closeMenu}
          onPlacedChange={setMenuPlaced}
          // The release panel is a dialog of its own, not a list of items to walk.
          onKeyDown={menu.kind === 'suppress' ? undefined : onMenuKeyDown}
        >
          {menu.kind === 'suppress' ? (
            suppressionPanel(menu)
          ) : menu.kind === 'group' ? (
            <>
              {/* Reshaping the folder tree is `ManageConfig`; suppressing it is not. Each item
                  asks for its own permission so an operator who may open a maintenance window on
                  a folder still gets that half of the menu (ADR-057). */}
              {/* Gated on its own prop, never on `canEdit`: any signed-in account may pin (ADR-146). */}
              {onTogglePin && (
                <button
                  type="button"
                  onClick={() => {
                    onTogglePin({ kind: 'group', id: menu.group.id });
                    setMenu(null);
                  }}
                >
                  {pins?.groups.has(menu.group.id) ? t('tree.unpin') : t('tree.pin')}
                </button>
              )}
              {canEdit && (
                <button type="button" onClick={() => { onAddGroup(menu.group.id); setMenu(null); }}>
                  {t('group.addSubgroup')}
                </button>
              )}
              {onAddNode && (
                <button type="button" onClick={() => { onAddNode(menu.group.id); setMenu(null); }}>
                  {t('tree.addNodeHere')}
                </button>
              )}
              {canEdit && (
                <button type="button" onClick={() => { onEditGroup(menu.group); setMenu(null); }}>
                  {t('tree.editMove')}
                </button>
              )}
              {/* Aim a sweep at this site's subnets (ADR-100 decision 10). It navigates rather
                  than acting: a sweep needs credentials and a poll-pool, and the Discovery screen
                  is where those are chosen — so the folder's prefixes arrive in the target field
                  and a person presses Start with the ranges in front of them. */}
              {onRunDiscovery && canRunDiscovery(menu.group, caps) && (
                <button
                  type="button"
                  onClick={() => {
                    onRunDiscovery(menu.group);
                    setMenu(null);
                  }}
                >
                  {t('tree.runDiscovery')}
                </button>
              )}
              {/* Arrange this folder's own children in name order (ADR-130). Two items rather
                  than one toggle: the tree has no "current sort" to toggle away from — the stored
                  order is whatever anyone last dragged it into — so a single item would have to
                  guess which direction the operator meant. `canEdit` is the same permission as
                  "Add subgroup" above, so this cannot be the only surviving item and
                  `groupMenuHasItems` needs no new field. */}
              {canEdit && (
                <>
                  <div className="ntree-menu-sep" />
                  <button
                    type="button"
                    onClick={() => {
                      onSortGroupChildren(menu.group.id, 'asc');
                      setMenu(null);
                    }}
                  >
                    {t('tree.sortAsc')}
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      onSortGroupChildren(menu.group.id, 'desc');
                      setMenu(null);
                    }}
                  >
                    {t('tree.sortDesc')}
                  </button>
                </>
              )}
              {poolMenu(
                { kind: 'group', id: menu.group.id, name: menu.group.name },
                menu.group.pool,
              )}
              {suppressionMenu(
                { kind: 'group', id: menu.group.id, name: menu.group.name },
                { kind: 'group', id: menu.group.id, name: menu.group.name },
                undefined,
                menu,
              )}
              {canEdit && (
                <>
                  <div className="ntree-menu-sep" />
                  <button type="button" className="danger" onClick={() => { onDeleteGroup(menu.group); setMenu(null); }}>
                    {t('common:actions.delete')}
                  </button>
                </>
              )}
            </>
          ) : menu.kind === 'node' ? (
            <>
              <button type="button" onClick={() => { onOpenNode(menu.node); setMenu(null); }}>
                {t('tree.open')}
              </button>
              {/* Poll now was reachable only from a node's detail header until ADR-124 増分 12,
                  which is the wrong place for it: it is what an operator presses right after
                  editing something in the tree. Batch-aware from the start, through the same
                  `nodeActionItems` answer as the moves. */}
              {onPollNodes && pollItems && (
                <button
                  type="button"
                  onClick={() => {
                    onPollNodes(
                      batchTarget ?? { kind: 'node', id: menu.node.id, name: menu.node.name },
                    );
                    setMenu(null);
                  }}
                >
                  {pollItems.scope === 'selection'
                    ? t('tree.pollSelected', { count: pollItems.count })
                    : pollItems.nameTheRow
                      ? t('tree.pollNodeNamed', { name: menu.node.name })
                      : t('tree.pollNow')}
                </button>
              )}
              {/* 🚨 These two act on the row even while the menu also carries
                  "Move N selected…" above them, so while a batch is in play they name the node
                  (ADR-124 増分 9 / ADR-055 R1). Neither has a batch form: a pin is this account's
                  own navigation and the edit dialog shows one node's whole binding. */}
              {onTogglePin && (
                <button
                  type="button"
                  onClick={() => {
                    onTogglePin({ kind: 'node', id: menu.node.id });
                    setMenu(null);
                  }}
                >
                  {pins?.nodes.has(menu.node.id)
                    ? actsOnBatch
                      ? t('tree.unpinNamed', { name: menu.node.name })
                      : t('tree.unpin')
                    : actsOnBatch
                      ? t('tree.pinNamed', { name: menu.node.name })
                      : t('tree.pin')}
                </button>
              )}
              {onEditNode && (
                <button type="button" onClick={() => { onEditNode(menu.node); setMenu(null); }}>
                  {actsOnBatch
                    ? t('tree.editNodeNamed', { name: menu.node.name })
                    : t('tree.editNodeEllipsis')}
                </button>
              )}
              {/* The move items, and what they act on (ADR-124 Inc.2). A right-click on a row that
                  is in the working set moves the working set and offers nothing that moves one —
                  the single item used to sit here above the batch item, and on a menu that ran off
                  the bottom of the screen it was the only one the operator could see. */}
              {moveItems?.scope === 'selection' && selectionMoveItems(moveItems.count)}
              {moveItems?.scope === 'row' && (
                <>
                  <button
                    type="button"
                    onClick={() => {
                      onRequestMoveNode(menu.node);
                      setMenu(null);
                    }}
                  >
                    {moveItems.nameTheRow
                      ? t('tree.moveNodeToGroup', { name: menu.node.name })
                      : t('tree.moveToGroup')}
                  </button>
                  {onMoveNodeByPrefix && canMoveByPrefix(groups, canEdit) && (
                    <button
                      type="button"
                      onClick={() => {
                        onMoveNodeByPrefix(menu.node);
                        setMenu(null);
                      }}
                    >
                      {moveItems.nameTheRow
                        ? t('tree.moveNodeByPrefix', { name: menu.node.name })
                        : t('tree.moveByPrefix')}
                    </button>
                  )}
                </>
              )}
              {/* The way in for someone who does not know the keyboard gesture (ADR-055 R6).
                  Ctrl / Shift is written nowhere on screen until a batch exists, so without this
                  item the feature is reachable only by people who were told about it. */}
              {onCheckedChange && (
                <button
                  type="button"
                  onClick={() => {
                    onCheckedChange(toggleChecked(checkedNodes, menu.node), menu.node.id);
                    setMenu(null);
                  }}
                >
                  {checkedNodes.has(menu.node.id)
                    ? t('tree.removeFromSelection')
                    : t('tree.addToSelection')}
                </button>
              )}
              {/* A batch exists but this row is not in it: the batch is still offered, below the
                  row's own (named) items, because the operator may well have meant it. */}
              {moveItems?.alsoSelection && (
                <>
                  <div className="ntree-menu-sep" />
                  {selectionMoveItems(moveItems.count)}
                </>
              )}
              {onAddNode && (
                <button type="button" onClick={() => { onAddNode(menu.node.group_id ?? null); setMenu(null); }}>
                  {t('tree.addNodeEllipsis')}
                </button>
              )}
              {/* The chips act on the batch when this row carries it (増分 10). `sharedOwnPool`
                  is what may mark a chip selected: reading this one row's pool would claim the
                  batch is set to it when most of the selection sits elsewhere. */}
              {poolMenu(
                batchTarget ?? { kind: 'node', id: menu.node.id, name: menu.node.name },
                batchTarget ? sharedOwnPool([...checkedNodes.values()]) : menu.node.pool,
              )}
              {/* The presets act on the batch when this row carries it (増分 11); the release
                  panel below them stays about the row, so it is passed the row's own node. */}
              {suppressionMenu(
                batchTarget ?? { kind: 'node', id: menu.node.id, name: menu.node.name },
                { kind: 'node', id: menu.node.id, name: menu.node.name },
                menu.node,
                menu,
              )}
              {/* What Delete acts on is `nodeTreeMenu.ts`'s call (ADR-124 増分 6): the working set
                  when this row is in it, otherwise this row — named while a set exists elsewhere. */}
              {deleteItems && (
                <>
                  <div className="ntree-menu-sep" />
                  {deleteItems.scope === 'selection' && onDeleteChecked ? (
                    <button type="button" className="danger" onClick={() => { onDeleteChecked(); setMenu(null); }}>
                      {t('tree.deleteSelected', { count: deleteItems.count })}
                    </button>
                  ) : (
                    onDeleteNode && (
                      <button type="button" className="danger" onClick={() => { onDeleteNode(menu.node); setMenu(null); }}>
                        {deleteItems.nameTheRow
                          ? t('tree.deleteNodeNamed', { name: menu.node.name })
                          : t('tree.deleteEllipsis')}
                      </button>
                    )
                  )}
                </>
              )}
            </>
          ) : (
            // kind === 'root': right-click on the Ungrouped header / empty tree → add at top level.
            onAddNode && (
              <button type="button" onClick={() => { onAddNode(null); setMenu(null); }}>
                {t('tree.addNodeHere')}
              </button>
            )
          )}
        </AnchoredPopover>
      )}
    </div>
  );
}
