// SPDX-License-Identifier: AGPL-3.0-only
// Nodes / All nodes — a two-pane split. The left pane is the inventory tree (groups → member
// nodes) with a per-group health rollup; selecting a row drives the right pane, which shows the
// chosen node's live detail (the shared <NodeDetail>, the same component as the /nodes/:id route)
// or a selected group's rollup. Triage and drill-in happen without leaving the inventory: pick a
// node, read its tabs, Poll now, move on. Add/rename/delete/move of groups and nodes runs through
// focused-edit modals (ManageConfig); 503 in skeleton mode is surfaced.
//
// Scale note: the tree is lazy (A-3). The initial view loads only the group skeleton + per-group
// health counts (`/fleet/group-summary`) + fleet totals — so the group rows and rollups paint
// instantly at any fleet size. A group's member nodes are fetched only once that group is **on
// screen** (`/nodes/by-group`), streaming in per group; a folder nobody has scrolled to is never
// loaded, and neither is a collapsed one. ⚠️ That used to say "open and visible", and meant it in
// the weaker sense of "no collapsed ancestor" — which, since collapse state defaults to empty, is
// every folder there is. A deployment with 500 of them asked for all 501 on first paint (ADR-125).
// The fetch set now comes from the rows the virtualizer is showing. An active name
// filter runs a debounced SERVER-side search (`/nodes?search=`), capped at one page, and drops the
// matches under their groups — it never loads the fleet into the browser. A term matching a GROUP's
// name additionally loads that folder's whole subtree, since the server search matches nodes and
// knows nothing about groups, and the folder's contents are what the operator asked for. The left pane is
// virtualized (only on-screen rows in the DOM, S13); node state stays live via the node-state SSE
// stream (`useNodeStates`) rather than a full refetch.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { api, errMsg } from '../services/api';
import { useCan, useNodeTabStore } from '../store';
import { usePrefsStore } from '../prefs';
import { setNodeTreePinnedOnly, setNodeTreeWithNodesOnly } from '../serverPrefs';
import { usePinsStore } from '../pinsStore';
import { pinnedView } from '../lib/pins';
import { PinIcon } from '../components/ui/icons';
import { useViewportMode } from '../lib/viewport';
import type {
  FleetGroupSummary,
  FleetSummary,
  MaintenanceWindow,
  Mute,
  NodeGroup,
  NodeSummary,
  PoolOption,
  SuppressionExemption,
} from '../types/api';
import { mergeNodesById, type StateCounts } from '../lib/nodeTree';
import { overlayLiveStates, type LiveOverlay } from '../lib/liveOverlay';
import { FILTER_SEARCH_LIMIT, useFilterSearch } from './useFilterSearch';
import { useUrlTerm } from '../lib/useUrlTerm';
import {
  inventoryColumns,
  inventoryFilterLabels,
  inventoryKey,
  isAttentionOnly,
  isInventoryFiltered,
  readInventoryFilters,
  toggleAttention,
  TREE_SEARCH_KEY,
  truncationNotice,
  writeInventoryFilters,
} from './inventoryFilters';
import { FilterBar } from '../components/ui/FilterBar';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { defaultFilters, type FilterState } from '../lib/columnFilter';
import { useLazyGroupMembers } from './useLazyGroupMembers';
import { addMenuTarget } from './nodesAddMenu';
import {
  LIVE_RECONCILE_MS,
  useNodeStateResyncs,
  useNodeStates,
} from '../dashboard/useNodeStates';
import {
  buildSuppressionIndex,
  nextSuppressionExpiry,
  suppressionRefreshDelayMs,
  suppressionPanelRows,
  releasableRows,
  type ReleaseAction,
  type SuppressionTarget,
} from '../lib/suppression';
import { inheritedGroupPool, sharedOwnPool } from '../lib/pool';
import { targetNodeIds, type ActionTarget } from '../lib/actionTarget';
import { escapeTarget, parseSelection, selectionToParam } from '../lib/treeSelection';
import { escapeClearsSelection } from '../lib/escapeDismiss';
import {
  maxTreeWidth,
  resolveTreeWidth,
  TREE_MIN_PX,
  widthFromDrag,
  widthFromKey,
} from './nodesPaneWidth';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { ActionMenu } from '../components/ui/ActionMenu';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { SearchField } from '../components/ui/SearchField';
import { AddNodeModal } from '../components/AddNodeModal/AddNodeModal';
import { GroupModal, type GroupModalState } from '../components/GroupModal/GroupModal';
import { NodeTree, type TreeSelection } from '../components/NodeTree/NodeTree';
import { canMoveByPrefix } from '../components/NodeTree/nodeTreeMenu';
import { NodeDetail, DeleteNodeModal } from '../components/NodeDetail/NodeDetail';
import { EditNodeModalById } from '../components/NodeDetail/EditNodeModal';
import { requestedNodeDetailTab } from '../components/NodeDetail/tabs';
import { GroupDetail } from '../components/NodeDetail/GroupDetail';
import { memberFetchState } from '../components/NodeDetail/groupMembers';
import { MoveNodeModal, type MoveTarget } from '../components/MoveNodeModal/MoveNodeModal';
import { MoveByPrefixModal } from '../components/MoveByPrefixModal/MoveByPrefixModal';
import { BulkTagModal } from '../components/NodeTree/BulkTagModal';
import { DeleteNodesModal } from '../components/NodeTree/DeleteNodesModal';
import { SetPoolModal } from '../components/SetPoolModal/SetPoolModal';
import { AddMaintenanceWindowModal } from '../components/suppression/AddMaintenanceWindowModal';
import { AddMuteModal } from '../components/suppression/AddMuteModal';
import './NodesPage.css';
import { groupDeletionImpact } from '../lib/nodeTree';

/** Stable empty per-group counts (avoids a fresh `{}` each render churning the tree memo). */
const EMPTY_GROUP_COUNTS: Record<string, StateCounts> = {};

export function NodesPage() {
  const { t } = useTranslation('nodes');
  const navigate = useNavigate();
  // Three permissions, not one signed-in flag: editing the inventory is ManageConfig, opening a
  // maintenance window is ManageMaintenance and muting is AckAlerts (`api/maintenance.rs`). They
  // were all `authed`, so a Viewer was offered every one of them (ADR-056 Inc.2).
  const canConfig = useCan('manage_config');
  const canMaintenance = useCan('manage_maintenance');
  const canAck = useCan('ack_alerts');
  const [groups, setGroups] = useState<NodeGroup[]>([]);
  // Server-side rollups: per-group direct counts drive the tree's group-row health bars (correct
  // over the whole fleet even before members load, A-1/A-3); the fleet summary drives the header
  // total + attention count without loading the inventory.
  const [groupSummary, setGroupSummary] = useState<FleetGroupSummary | null>(null);
  const [fleetSummary, setFleetSummary] = useState<FleetSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  /** The folders the tree says are on screen and still waiting for members (ADR-125). The tree
   *  publishes it from the rows the virtualizer is showing; this page only relays it to the member
   *  cache. It used to be the collapse prefs, and the hook derived "open, with no collapsed
   *  ancestor" — which defaults to every folder, so a 500-folder deployment asked for all of them
   *  on first paint. `NodeTree` publishes it only once the viewport has settled, so this state
   *  changes when the answer really does rather than on every scroll frame. */
  const [visibleGroupKeys, setVisibleGroupKeys] = useState<string[]>([]);
  // Inventory-pane collapse (desktop only): slim the tree to a rail so the detail uses the full
  // width. On mobile the pane switcher governs, so the rail is suppressed there.
  const paneCollapsed = usePrefsStore((s) => s.nodesPaneCollapsed);
  const toggleNodesPane = usePrefsStore((s) => s.toggleNodesPane);
  const isMobileView = useViewportMode() === 'mobile';
  const railed = paneCollapsed && !isMobileView;

  // ── Split resize (ADR-074) ────────────────────────────────────────────────────────────────────
  // The handle between the two panes. Arithmetic in `nodesPaneWidth.ts`; only the pointer plumbing
  // is here, because Vitest cannot reach a `.tsx`.
  const storedWidth = usePrefsStore((s) => s.nodesPaneWidth);
  const setStoredWidth = usePrefsStore((s) => s.setNodesPaneWidth);
  // The width while a gesture is in flight, kept local so a drag does not write to the persisted
  // store once per frame — the store gets one write on release.
  const [liveWidth, setLiveWidth] = useState<number | null>(null);
  const [splitEl, setSplitEl] = useState<HTMLDivElement | null>(null);
  const [splitPx, setSplitPx] = useState(0);
  const drag = useRef<{ x: number; w: number } | null>(null);
  const dragRaf = useRef(0);
  const pointerX = useRef(0);
  const treeWidth = resolveTreeWidth(liveWidth ?? storedWidth, splitPx);

  // Measure the space the two panes share. Observes `.nodes-split` itself, whose width is set by
  // the page column — nothing inside it can change that, so this observer cannot loop with the
  // column widths it feeds.
  useEffect(() => {
    if (!splitEl) return;
    const measure = () => setSplitPx(splitEl.clientWidth);
    measure();
    if (typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver(measure);
    ro.observe(splitEl);
    return () => ro.disconnect();
  }, [splitEl]);

  const onSplitDown = useCallback(
    (e: React.PointerEvent) => {
      (e.target as Element).setPointerCapture?.(e.pointerId);
      e.preventDefault(); // no text selection across the tree while dragging
      drag.current = { x: e.clientX, w: treeWidth };
      pointerX.current = e.clientX;
      setLiveWidth(treeWidth);
    },
    [treeWidth],
  );

  const onSplitMove = useCallback(
    (e: React.PointerEvent) => {
      if (!drag.current) return;
      pointerX.current = e.clientX;
      // Coalesce to one update per frame: pointermove can outrun paint, and each update relayouts
      // the virtualized tree and the whole detail pane. Same shape as the Interfaces dock.
      cancelAnimationFrame(dragRaf.current);
      dragRaf.current = requestAnimationFrame(() => {
        const d = drag.current;
        if (!d) return;
        setLiveWidth(widthFromDrag(d.w, d.x, pointerX.current, splitPx));
      });
    },
    [splitPx],
  );

  const endSplitDrag = useCallback(
    (e: React.PointerEvent) => {
      const d = drag.current;
      if (!d) return;
      (e.target as Element).releasePointerCapture?.(e.pointerId);
      cancelAnimationFrame(dragRaf.current);
      const final = widthFromDrag(d.w, d.x, pointerX.current, splitPx);
      drag.current = null;
      setLiveWidth(null);
      setStoredWidth(final);
    },
    [splitPx, setStoredWidth],
  );

  const onSplitKey = useCallback(
    (e: React.KeyboardEvent) => {
      // Keyboard-operable, like every other primary control (ui-conventions.md). ⚠️ This handle is
      // horizontal — ArrowLeft/Right — which is neither of the two that came before it.
      const next = widthFromKey(treeWidth, e.key, splitPx);
      if (next == null) return;
      e.preventDefault();
      setStoredWidth(next);
    },
    [treeWidth, splitPx, setStoredWidth],
  );

  // The right-pane selection and the inline detail tab live in the URL (`?sel=node:<id>&tab=…`)
  // so a browser reload restores the same pane instead of snapping back to the empty state
  // (design-guidelines.md "画面状態の永続化"). So does the left pane's search term, since ADR-153.
  const [searchParams, setSearchParams] = useSearchParams();
  const selected: TreeSelection = parseSelection(searchParams.get('sel'));
  const tabParam = searchParams.get('tab') ?? '';
  // The URL wins when it names a tab (a reload, a shared link); otherwise the tab the operator last
  // clicked does (ADR-134). That is what makes `select()` below able to drop `tab` and still open
  // the pane on Interfaces — which is the point: comparing one tab across a stack of switches used
  // to mean re-clicking it on every row.
  const remembered = useNodeTabStore((s) => s.tab);
  const tab = requestedNodeDetailTab(tabParam, remembered);
  // The search box (ADR-153). The box itself is a local draft; the URL receives the term once the
  // typing settles — `useFilterSearch` decides when, and the effect below it commits. Until ADR-153
  // this was plain component state and a reload threw the term away, which left three of the four
  // controls on this row surviving a reload and the one an operator types into not.
  const term = useUrlTerm(TREE_SEARCH_KEY);
  const filter = term.draft;
  // Pick a row → write the selection and drop the tab param, so the pane opens on the tab the
  // operator last clicked rather than on whatever the *previous* row's URL happened to say
  // (`requestedNodeDetailTab`). Dropping it is still right: a link to one node's Flow tab must not
  // silently become a link to another node's.
  // `replace` keeps rapid clicking out of the browser history.
  const select = useCallback(
    (sel: TreeSelection) => {
      const params = new URLSearchParams(searchParams);
      const value = selectionToParam(sel);
      if (value) params.set('sel', value);
      else params.delete('sel');
      params.delete('tab');
      setSearchParams(params, { replace: true });
    },
    [searchParams, setSearchParams],
  );

  /** The working set: nodes checked with Ctrl / Shift, for a bulk action (ADR-124 決定 2).
   *
   *  ⚠️ **Whole nodes, not ids, and deliberately not in the URL.** Ids resolved against the
   *  current rows would shrink the batch the moment a filter changed; and a set restored from a
   *  link would name rows the reloaded tree has not fetched — which is the state ADR-073 removed. */
  const [checked, setChecked] = useState<Map<string, NodeSummary>>(new Map());
  /** Row a Shift click measures its range from. An id, never an index: the flat row list is
   *  rebuilt on every SSE frame, filter change and lazy load. */
  const [anchorId, setAnchorId] = useState<string | null>(null);
  const clearChecked = useCallback(() => {
    setChecked(new Map());
    setAnchorId(null);
  }, []);

  // Escape clears the selection (ADR-073). Before this the split had no desktop way back to the
  // empty right pane at all: `select(null)` existed but its only caller was the mobile pane
  // switcher's back chevron, which `.nodes-detail-back { display: none }` hides on a desktop.
  //
  // The guard is shared rather than spelled here, because the dashboard's edit mode and the
  // Interfaces dock ask the same question and three copies would drift. It answers false while a
  // modal, a popover or the tree's context menu is open — those own the press — and false while the
  // operator is typing, so Escape still belongs to the pane's search box.
  //
  // ⚠️ Since ADR-124 the page has **two** things Escape can clear, and this is deliberately still
  // one listener. Two would each answer for themselves and both fire on the same press, clearing
  // the working set and the pane at once — which reads as two separate bugs. The order lives in
  // `escapeTarget`, where a test runs it.
  useEffect(() => {
    if (!selected && checked.size === 0) return;
    const onKey = (e: KeyboardEvent) => {
      if (!escapeClearsSelection(e)) return;
      const target = escapeTarget(checked.size > 0, !!selected);
      if (target === 'checked') clearChecked();
      else if (target === 'selection') select(null);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [selected, select, checked, clearChecked]);

  const setTab = useCallback(
    (next: string) => {
      const params = new URLSearchParams(searchParams);
      params.set('tab', next);
      setSearchParams(params, { replace: true });
    },
    [searchParams, setSearchParams],
  );

  // Add-node modal.
  const [adding, setAdding] = useState(false);
  /** Folder the new node lands in (group_id): set from the right-clicked group/node; `null` = top
   *  level. createNode itself takes no group_id, so on success we follow up with setNodeGroup. */
  const [addGroupId, setAddGroupId] = useState<string | null>(null);

  // Group + move modals.
  const [groupModal, setGroupModal] = useState<GroupModalState | null>(null);
  const [deletingGroup, setDeletingGroup] = useState<NodeGroup | null>(null);
  const [deletingNode, setDeletingNode] = useState<NodeSummary | null>(null);
  /** The working set the bulk delete is about — from the tree's menu or the selection bar
   *  (ADR-124 増分 6). `null` ⇒ closed. */
  const [deletingNodes, setDeletingNodes] = useState<NodeSummary[] | null>(null);
  /** Nodes the move dialog is about: one from the tree's own "Move…", or the whole working set
   *  from the selection bar. `null` ⇒ closed. */
  const [moving, setMoving] = useState<MoveTarget[] | null>(null);
  /** Nodes the IP-range proposal is about. Same two entry points, same shape (ADR-124 決定 6). */
  const [movingByPrefix, setMovingByPrefix] = useState<NodeSummary[] | null>(null);
  const [taggingNodes, setTaggingNodes] = useState<NodeSummary[] | null>(null);
  /** Node whose edit dialog is open, from the tree's right-click. The row is all this page has, so
   *  the dialog loads the detail itself (`EditNodeModalById`) — like Delete/Move above, editing does
   *  not move the selection, so the right pane keeps showing whatever the operator was looking at. */
  const [editingNode, setEditingNode] = useState<NodeSummary | null>(null);
  /** Bumped when that edit rewrote the node the right pane is showing. It rides in `NodeDetail`'s
   *  `key`: the pane fetches a node's config once per mount (plus its own post-save refetch), so
   *  without this an edit from the tree would leave the operator looking at the values they just
   *  replaced — which reads as a save that did not take. */
  const [detailNonce, setDetailNonce] = useState(0);

  // Suppression: active maintenance windows + mutes drive the per-row icons; the right-click
  // "Custom…" path opens a prefilled create modal (preset durations POST directly).
  const [windows, setWindows] = useState<MaintenanceWindow[]>([]);
  const [mutes, setMutes] = useState<Mute[]>([]);
  // Nodes an operator has released from a suppression they only inherited. Loaded with the two
  // lists above because a released node is *not* suppressed — the tree needs all three to draw the
  // markers correctly, and the release panel needs them to offer the undo.
  const [exemptions, setExemptions] = useState<SuppressionExemption[]>([]);
  const [maintenanceTarget, setMaintenanceTarget] = useState<ActionTarget | null>(null);
  const [muteTarget, setMuteTarget] = useState<ActionTarget | null>(null);
  // Poll-pool assignment from the tree's right-click chips (ADR-009/020). `pools` feeds the chips;
  // `poolTarget` holds the target whose "Custom…" dialog is open.
  const [pools, setPools] = useState<PoolOption[]>([]);
  const [poolTarget, setPoolTarget] = useState<ActionTarget | null>(null);
  /** The transient result of a manual poll (ADR-124 増分 12). Its own line rather than the page's
   *  error band, because a dispatch that worked is the only feedback there is. */
  const [pollMsg, setPollMsg] = useState<{ text: string; tone: 'info' | 'error' } | null>(null);
  // Per-group direct counts (server rollup) → the tree's group-row health bars + the header stats.
  const groupCounts = groupSummary?.groups ?? EMPTY_GROUP_COUNTS;

  // The three controls live in the URL beside the selection — they are the part someone shares
  // ("the URL monitors in the tokyo pool"), and a reload that dropped them would silently widen
  // the list back to the whole fleet. The search term joined them in ADR-153, for the same reason.
  //
  // Since ADR-053 Inc.6 each takes a **set**, so "everything that is not healthy" is one question
  // rather than three separate looks at the tree. Declared here rather than beside the other URL
  // state because `pools` is a dependency: pool names are the deployment's own, not an enum.
  const filterCols = useMemo(() => inventoryColumns(t, pools), [t, pools]);
  const filterLabels = useMemo(() => inventoryFilterLabels(t), [t]);
  const [filterSheet, setFilterSheet] = useState(false);
  const inventoryFilters = readInventoryFilters(filterCols, searchParams);
  const setInventoryFilters = useCallback(
    (next: FilterState) => {
      const params = new URLSearchParams(searchParams);
      writeInventoryFilters(filterCols, params, next);
      setSearchParams(params, { replace: true });
    },
    [filterCols, searchParams, setSearchParams],
  );

  // "Needs attention" (ADR-163) — the one press that asks the question this screen is opened for.
  // It selects warning + critical + unreachable in the `state` filter above, which is why nothing
  // else on this page had to learn about it: `isInventoryFiltered`, `ClearFilters` and
  // `clearAllFilters` already watch that column. Compare with Pinned only and With nodes, which
  // hold switches of their own and therefore had to be taught to all three (ADR-159 決定 8).
  const attentionOnly = isAttentionOnly(inventoryFilters);
  const pressAttention = useCallback(
    (fromHeader: boolean) => {
      // ⚠️ `attentionOnly` is read before the write, so this is "was it on", not "is it on".
      const turningOn = !attentionOnly;
      setInventoryFilters(toggleAttention(inventoryFilters));
      // The header count stays on screen while the inventory is railed to a 40px strip, so pressing
      // it there would narrow a tree nobody can see — a control that reads as having done nothing
      // (ADR-055 R6). Only on the way ON: opening the pane as a side effect of *clearing* a filter
      // would be a second, unasked-for change.
      if (fromHeader && turningOn && railed) toggleNodesPane();
    },
    [attentionOnly, inventoryFilters, setInventoryFilters, railed, toggleNodesPane],
  );

  // Pins (ADR-146): this account's own, from the server, and the switch that narrows the tree to
  // them. The switch only counts once the pins have loaded — a core without the endpoint draws no
  // button, and must not narrow the tree to nothing because the account said "on" elsewhere.
  const pinsStatus = usePinsStore((s) => s.status);
  const pinGroupIds = usePinsStore((s) => s.groupIds);
  const pinNodeIds = usePinsStore((s) => s.nodeIds);
  const pinNodes = usePinsStore((s) => s.nodes);
  const pinsReady = pinsStatus === 'ready';
  const storedPinnedOnly = usePrefsStore((s) => s.nodeTreePinnedOnly) === true;
  const pinnedOnly = pinsReady && storedPinnedOnly;
  const pins = useMemo(
    () => (pinsReady ? pinnedView(groups, pinGroupIds, pinNodeIds, pinNodes) : undefined),
    [pinsReady, groups, pinGroupIds, pinNodeIds, pinNodes],
  );
  // Re-read on every visit: pins set from another machine since sign-in show up here.
  useEffect(() => {
    void usePinsStore.getState().load();
  }, []);

  // Folders with nodes only (ADR-159): hide every folder with no node below it. Held on the account
  // like Pinned only, so it survives a reload — which is what makes the next line necessary.
  const withNodesOnly = usePrefsStore((s) => s.nodeTreeWithNodesOnly) === true;
  /** Folders created on this screen since it was opened. A folder is empty the moment it is made,
   *  so without this the operator presses "New folder" and the tree does not change — which reads
   *  as a create that failed, not as a filter doing its job (ADR-159, ADR-055 R6).
   *
   *  ⚠️ Deliberately forgotten on reload: it answers "you just made this", not "this folder is
   *  special". By then the folder has either been filled or is one the switch is right to hide. */
  const [createdGroups, setCreatedGroups] = useState<ReadonlySet<string>>(() => new Set());

  // Members load lazily, per group, only once that group's contents are on screen (A-3). The hook
  // owns that cache; this page only says what is currently worth having loaded.
  // Any of the four puts the tree into filter mode. The state / kind / pool ones count even with
  // an empty box: "show me the URL monitors" is a whole question, and browsing the folder tree
  // while one is set would show every node and look like the control did nothing.
  const filtering = filter.trim().length > 0 || isInventoryFiltered(inventoryFilters);
  // Clearing everything is one handler, and it writes the URL exactly once: the three controls and
  // the search term go into the same `URLSearchParams`. Two `setSearchParams` calls from one render
  // snapshot and the second restores what the first cleared — that bug has already shipped once
  // (`ClearFilters`' own doc). `term.assign` also records the cleared term, so the commit the
  // settle triggers next finds nothing to write rather than writing from the pre-clear snapshot.
  const assignTerm = term.assign;
  const clearAllFilters = useCallback(() => {
    // Pinned only narrows the tree too, so "clear all filters" that left it on would be untrue.
    if (pinnedOnly) setNodeTreePinnedOnly(false);
    // Same for Folders with nodes only (ADR-159) — it is hiding rows, so it is a filter.
    if (withNodesOnly) setNodeTreeWithNodesOnly(false);
    const params = new URLSearchParams(searchParams);
    writeInventoryFilters(filterCols, params, defaultFilters(filterCols));
    assignTerm(params, '');
    setSearchParams(params, { replace: true });
  }, [filterCols, pinnedOnly, withNodesOnly, searchParams, setSearchParams, assignTerm]);
  // Filter mode's server-side page — the nodes that matched. One capped page, never the fleet; the
  // folders a group-name match reveals arrive separately through the per-group member cache below.
  // `appliedTerm` is the debounced term the search was issued for, so the reveal loads in step with
  // the search rather than once per keystroke.
  const search = useFilterSearch(filter, inventoryFilters);
  const refetchSearch = search.refetch;
  // The settled term goes to the URL. Keyed on the settled value ALONE — `commit` is stable, and an
  // effect that also re-ran on URL changes would write the old term back over a Back navigation.
  const commitTerm = term.commit;
  const settledTerm = search.settledTerm;
  useEffect(() => commitTerm(settledTerm), [commitTerm, settledTerm]);
  const members = useLazyGroupMembers({
    groups,
    visibleGroupKeys,
    ready: !loading,
    browsing: !filtering,
    selectedGroupId: selected?.kind === 'group' ? selected.id : null,
    filterTerm: search.appliedTerm,
  });
  const invalidateMembers = members.invalidate;

  // The nodes the tree renders, in three cases rather than two.
  //
  //  - **browsing** — the lazily-loaded per-group members.
  //  - **a text term only** — those members merged with the server search's capped page, so a
  //    folder matched by *name* can show its contents (and a selected group can still roll its
  //    subtree up). Deduped by id; `flatRowKey` is `n:<id>`. The merge is safe here because
  //    `flattenTree` hides every row that does not match the term, so the extra members cost
  //    nothing on screen.
  //  - **a state / kind / pool filter** — the server's page ALONE.
  //
  // 🚨 That third case is a fix, not a refinement. `flattenTree` narrows by the **text term only**
  // (`lib/nodeTree.ts::filterTerm`), so with an empty box it hides nothing — and the merge handed
  // it back every member already cached from browsing, none of which the state/kind/pool filter
  // had ever been applied to. Picking "Critical" after expanding a few folders left the whole tree
  // on screen. It is wrong with a term too: the server narrowed its page by text *and* state,
  // while the members were narrowed by neither, so a node matching the text but not the state
  // would survive the merge.
  const serverNarrowed = isInventoryFiltered(inventoryFilters);
  //
  // Pinned only (ADR-146) adds the pinned nodes themselves: a pinned node usually sits in a folder
  // nobody has loaded. Never under a state / kind / pool filter — those rows have not passed it, which
  // is the same reason the members are not merged there.
  const treeNodes = useMemo(() => {
    const base = serverNarrowed
      ? search.nodes
      : filtering
        ? mergeNodesById(search.nodes, members.nodes)
        : members.nodes;
    return pinnedOnly && !serverNarrowed ? mergeNodesById(base, [...pinNodes]) : base;
  }, [serverNarrowed, filtering, search.nodes, members.nodes, pinnedOnly, pinNodes]);

  // Overlay the live SSE node states (S14) so the tree's status dots update without re-fetching.
  // `live` publishes a new Map on every flush (any node in the FLEET, ~10×/s during a first-observe
  // burst), and this array is `NodeTree`'s `buildNodeTree` + `flattenTree` memo key — so a plain
  // `.map` re-built the whole tree on flushes where not one loaded node had moved. The ref carries
  // the previous result and hands the same array back when nothing visible changed.
  const live = useNodeStates();
  const overlay = useRef<LiveOverlay<NodeSummary> | null>(null);
  const liveTreeNodes = useMemo(() => {
    overlay.current = overlayLiveStates(treeNodes, live, overlay.current);
    return overlay.current.out;
  }, [treeNodes, live]);

  const suppression = useMemo(
    () => buildSuppressionIndex(windows, mutes, groups, treeNodes, exemptions),
    [windows, mutes, groups, treeNodes, exemptions],
  );

  // Load the group skeleton + server rollups (fast at any fleet size). Members load lazily below.
  //
  // 🚨 **Three independent settles, not one `Promise.all`** (ADR-133). The tree paints from the
  // folder list alone, and `useLazyGroupMembers` cannot start until `loading` clears — so waiting
  // for all three made the first member fetch wait on the SLOWEST of them, which is the per-group
  // rollup: a full scan of `nodes`, plus a fleet-wide TSDB freshness query whenever the alert
  // engine has no opinion about someone. The bars and the header totals fill in a round trip later.
  //
  // ⚠️ **Only the folder list is awaited, and that is deliberate.** `moveNodes` awaits this call
  // before reporting a partial move, and what it needs current is the tree — not the health bars.
  /** The two server-computed rollups: per-folder tallies and the fleet total.
   *
   *  ⚠️ A failure leaves the previous answer in place — see the note inside `reload`. */
  const refreshRollups = useCallback(() => {
    api.getFleetGroupSummary().then(setGroupSummary).catch(() => undefined);
    api.getFleetSummary().then(setFleetSummary).catch(() => undefined);
  }, []);

  const reload = useCallback(async () => {
    setError(null);
    // ⚠️ **A failure leaves the previous answer in place, and `null` when there was none**
    // (ADR-133). This used to substitute `{}`, which is not "no answer" — it is the valid answer
    // "every folder is empty". Read as one, it stopped the lazy load dead: no counts, therefore no
    // `group-loading` rows, therefore nothing asking for members, therefore every folder drawn as
    // empty — for as long as the page stayed open. `countsPending` below says "not answered"
    // instead, and the members arrive whether or not this endpoint ever does.
    refreshRollups();
    try {
      const g = await api.listNodeGroups();
      setGroups(g);
      // Both member caches are now stale, and both have to be told so. Dropping the per-group
      // members alone left filter mode showing an empty tree after any edit: the search page was
      // cleared but nothing re-issued the search, so it stayed cleared until the operator retyped
      // the term. They are invalidated the same way, side by side, for that reason.
      invalidateMembers();
      refetchSearch();
    } catch (e: unknown) {
      setError(errMsg(e, t('err.loadNodes')));
    } finally {
      setLoading(false);
    }
  }, [t, invalidateMembers, refetchSearch, refreshRollups]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // 🚨 The rollups used to be read on mount and after a write, and never again. The stream moves
  // the row dots and nothing else, so a tab left open showed red dots under a green folder bar,
  // with a header count from whenever it was opened. Re-read on the same slow clock the topology
  // views reconcile on. Members are NOT re-read — that is what the viewport-driven fetch is for.
  useEffect(() => {
    const id = setInterval(refreshRollups, LIVE_RECONCILE_MS);
    return () => clearInterval(id);
  }, [refreshRollups]);

  // …and at once when the stream says it missed frames. `useNodeStates` has already dropped its
  // overlay by then, so what is on screen is the base data — which has to be current.
  //
  // 🚨 **In place, never `reload`** (ADR-133 増分 7 決定 4). `reload` is for after a write: it
  // empties the member cache, so for a round trip every loaded row was gone — the tree flashed
  // "Loading…" on each reconnect and an arrow key pressed meanwhile was dropped. What was missed is
  // states, not moves and not folders, so the folder list is not re-read either.
  //
  // Counted against the resyncs this page has SEEN: a new callback identity is not a new resync, and
  // one that happened on another screen before this one mounted is not replayed (the mount reads
  // everything anyway).
  const refreshMembers = members.refresh;
  const resyncs = useNodeStateResyncs();
  const seenResyncs = useRef(resyncs);
  useEffect(() => {
    if (resyncs === seenResyncs.current) return;
    seenResyncs.current = resyncs;
    refreshRollups();
    refreshMembers();
    refetchSearch();
  }, [resyncs, refreshRollups, refreshMembers, refetchSearch]);

  // Active maintenance windows + mutes for the per-row suppression icons. Refetched after any
  // maintenance/mute action from the tree so the icons update immediately (node `maintenance`
  // state catches up on the engine's next ~30s refresh).
  const reloadSuppression = useCallback(() => {
    api.listMaintenanceWindows().then(setWindows).catch(() => undefined);
    api.listMutes().then(setMutes).catch(() => undefined);
    api.listSuppressionExemptions().then(setExemptions).catch(() => undefined);
  }, []);

  useEffect(() => {
    reloadSuppression();
  }, [reloadSuppression]);

  // …and again the moment the next one runs out. Without this the three lists were fetched on
  // mount and after an action and never again, so a tab left open kept drawing the wrench and the
  // bell for windows that had ended hours earlier. A timer on the earliest expiry costs nothing
  // while nothing is expiring — and it schedules only instants still ahead, so a server whose
  // clock disagrees cannot turn this into a refetch loop. `buildSuppressionIndex` already ignores
  // anything past, so a late or failed refetch leaves the markers correct, not stale.
  // ⚠️ The delay comes from `suppressionRefreshDelayMs`, never from a subtraction here: an expiry
  // more than 24.8 days out overflows `setTimeout`, and that WAS a refetch loop.
  useEffect(() => {
    const at = nextSuppressionExpiry(windows, mutes, exemptions);
    if (at === null) return undefined;
    const h = setTimeout(reloadSuppression, suppressionRefreshDelayMs(at));
    return () => clearTimeout(h);
  }, [windows, mutes, exemptions, reloadSuppression]);

  // Right-click → maintenance/mute. A preset duration POSTs now → now+duration immediately; the
  // "Custom…" item (durationMs === null) opens the full create form prefilled with the scope.
  const scopeError = (e: unknown, fallback: string) => setError(errMsg(e, fallback));

  /** Right-click / More… → poll now (ADR-124 増分 12).
   *
   *  ⚠️ **Not the page's error band.** A dispatch that worked is news too — "3 of 3 queued" is the
   *  only feedback there is, since the results arrive minutes later through the ordinary ingest —
   *  so this gets its own transient line, the same shape as the node detail header's `pollMsg`.
   *  A failure stays until the next attempt; a success clears itself. */
  const pollNodes = useCallback(
    (target: ActionTarget) => {
      const ids = targetNodeIds(target);
      if (ids.length === 0) return;
      setPollMsg(null);
      api
        .pollNodes(ids)
        .then((r) => {
          if (target.kind === 'nodes') clearChecked();
          setPollMsg({
            text: t('tree.pollDispatched', { dispatched: r.dispatched, requested: r.requested }),
            tone: r.dispatched < r.requested ? 'error' : 'info',
          });
          // A shortfall is left on screen: it means nodes the operator selected were not polled.
          if (r.dispatched >= r.requested) {
            window.setTimeout(() => setPollMsg(null), 8000);
          }
        })
        .catch((e: unknown) =>
          setPollMsg({ text: errMsg(e, t('err.requestPoll')), tone: 'error' }),
        );
    },
    // ⚠️ Stable, because `selectionMenuItems` memoizes on it — an inline definition would rebuild
    // that array on every render, which on this page means every SSE node-state frame.
    [t, clearChecked],
  );

  const setMaintenance = (target: ActionTarget, durationMs: number | null) => {
    if (durationMs === null) {
      setMaintenanceTarget(target);
      return;
    }
    const now = new Date();
    const ends = new Date(now.getTime() + durationMs).toISOString();
    if (target.kind === 'nodes') {
      // One window per node, because a window's scope is a single id. The partial result goes on
      // the page's error band, **after** the refresh — reporting it first would be wiped by
      // `reloadSuppression`'s own `setError(null)` and a half-covered fleet would read as covered.
      api
        .createMaintenanceWindows({
          node_ids: targetNodeIds(target),
          name: t('maintenanceWindowNameMany', { count: target.nodes.length }),
          starts_at: now.toISOString(),
          ends_at: ends,
        })
        .then(async (r) => {
          clearChecked();
          await reloadSuppression();
          if (r.created < r.requested) setError(t('err.setMaintenancePartial', { ...r }));
        })
        .catch((e: unknown) => scopeError(e, t('err.setMaintenance')));
      return;
    }
    api
      .createMaintenanceWindow({
        name: t('maintenanceWindowName', { name: target.name }),
        scope_level: target.kind === 'group' ? 'group_id' : 'node',
        scope_id: target.id,
        starts_at: now.toISOString(),
        ends_at: ends,
      })
      .then(reloadSuppression)
      .catch((e: unknown) => scopeError(e, t('err.setMaintenance')));
  };

  const setMute = (target: ActionTarget, durationMs: number | null) => {
    if (durationMs === null) {
      setMuteTarget(target);
      return;
    }
    if (target.kind === 'nodes') {
      api
        .createMutes({
          node_ids: targetNodeIds(target),
          until: new Date(Date.now() + durationMs).toISOString(),
        })
        .then(async (r) => {
          clearChecked();
          await reloadSuppression();
          if (r.created < r.requested) setError(t('err.mutePartial', { ...r }));
        })
        .catch((e: unknown) => scopeError(e, t('err.mute')));
      return;
    }
    api
      .createMute({
        scope_kind: target.kind === 'group' ? 'group' : 'node',
        scope_id: target.id,
        until: new Date(Date.now() + durationMs).toISOString(),
      })
      .then(reloadSuppression)
      .catch((e: unknown) => scopeError(e, t('err.mute')));
  };

  // What the release panel shows for a tree row. Resolved here because this page holds the window,
  // mute and exemption lists; the tree renders the answer and owns none of the data (its header
  // comment). The exemptions are what stop a released node being offered a release it already has.
  const suppressionRows = useCallback(
    (target: SuppressionTarget, node?: NodeSummary) =>
      releasableRows(suppressionPanelRows(target, { windows, mutes, groups, node, exemptions }), (p) =>
        p === 'manage_maintenance' ? canMaintenance : canAck,
      ),
    [windows, mutes, groups, exemptions, canMaintenance, canAck],
  );

  // Right-click / marker click → release. One exhaustive switch, so a new `ReleaseAction` cannot
  // ship half-wired. Every arm refreshes the suppression lists; releasing a *node* also touches
  // the node's rolled-up state, which the engine recomputes on its own ~30s cycle — the markers
  // update immediately from the exemption list, the status dot catches up with the engine.
  const release = (a: ReleaseAction) => {
    const call = (() => {
      switch (a.action) {
        case 'end-window':
          return api.endMaintenanceWindow(a.windowId);
        case 'lift-mute':
          return api.deleteMute(a.muteId);
        case 'release-node':
          return a.kind === 'maintenance'
            ? api.setNodeMaintenanceExemption(a.nodeId, true)
            : api.setNodeMuteExemption(a.nodeId, true);
        case 'undo-release':
          return a.kind === 'maintenance'
            ? api.setNodeMaintenanceExemption(a.nodeId, false)
            : api.setNodeMuteExemption(a.nodeId, false);
      }
    })();
    call.then(reloadSuppression).catch((e: unknown) => scopeError(e, t('err.release')));
  };

  // The pools the chips offer. Cheap (two indexed DISTINCTs server-side), so it is refreshed with
  // the inventory rather than polled; a failure just leaves the chips empty — "Custom…" still works.
  const reloadPools = useCallback(() => {
    api
      .listPools()
      .then((r) => setPools(r.pools))
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    reloadPools();
  }, [reloadPools]);

  // Right-click → assign a poll-pool. A chip writes immediately (like the suppression presets);
  // "Custom…" (pool === null) opens the dialog for a pool that doesn't exist yet. Assigning a
  // folder re-pools every node beneath it that has no pool of its own.
  const setPool = (target: ActionTarget, pool: string | null) => {
    if (pool === null) {
      setPoolTarget(target);
      return;
    }
    // 🚨 A chip has no dialog to hold a partial result in, so the page says it — and **after** the
    // refresh, never before: `reload` opens with `setError(null)`, so a shortfall reported first is
    // wiped in the same tick and a batch that moved half reads as a clean success. That is the
    // failure the endpoint returns two numbers to prevent (ADR-124 増分 4's lesson, 増分 10).
    if (target.kind === 'nodes') {
      api
        .setNodesPool(targetNodeIds(target), pool)
        .then(async (r) => {
          clearChecked();
          reloadPools();
          await reload();
          if (r.applied < r.requested) setError(t('err.setPoolPartial', { ...r }));
        })
        .catch((e: unknown) => scopeError(e, t('err.setPool')));
      return;
    }
    const call =
      target.kind === 'node'
        ? api.setNodePool(target.id, pool)
        : api.setNodeGroupPool(target.id, pool);
    call
      .then(() => {
        reloadPools();
        return reload();
      })
      .catch((e: unknown) => scopeError(e, t('err.setPool')));
  };

  /** The working set's less-common verbs, for the selection bar's "More…" menu.
   *
   *  ⚠️ Each entry is gated on the permission **its own handler's `Require*` checks**, never on
   *  `canConfig` for the lot (ADR-056): the pool write is `ManageConfig`, while the suppression
   *  writes are `ManageMaintenance` and `AckAlerts`, which an Operator holds separately. Gating the
   *  menu on its strictest member is the mistake that once removed the tree's whole context menu.
   *  The menu itself is not rendered when nothing survives. */
  const selectionMenuItems = useMemo(() => {
    const nodes = [...checked.values()];
    const items: { key: string; label: string; onSelect: () => void }[] = [];
    if (canConfig) {
      items.push({
        key: 'pool',
        label: t('select.pool'),
        onSelect: () => setPoolTarget({ kind: 'nodes', nodes }),
      });
    }
    if (canConfig) {
      items.push({
        key: 'poll',
        label: t('select.pollNow'),
        onSelect: () => pollNodes({ kind: 'nodes', nodes }),
      });
    }
    if (canMaintenance) {
      items.push({
        key: 'maintenance',
        label: t('select.maintenance'),
        onSelect: () => setMaintenanceTarget({ kind: 'nodes', nodes }),
      });
    }
    if (canAck) {
      items.push({
        key: 'mute',
        label: t('select.mute'),
        onSelect: () => setMuteTarget({ kind: 'nodes', nodes }),
      });
    }
    return items;
  }, [checked, canConfig, canMaintenance, canAck, pollNodes, t]);

  // Once loaded, validate the URL selection: keep it if the entity still exists; otherwise fall
  // back to the first problem node (warning/critical/unreachable), else clear it. The fallback is
  // written back to the URL (replace) so a reload lands on the same pane. Runs only when the
  // current selection is missing/stale, so it can't fight a user's choice.
  // If the current selection is a group that no longer exists, clear it. A node selection is left
  // as-is — the lazy tree doesn't hold the whole inventory to validate against, and the detail pane
  // fetches the node by id and surfaces a missing one itself.
  useEffect(() => {
    if (loading) return;
    const cur = parseSelection(searchParams.get('sel'));
    if (!cur || cur.kind !== 'group' || groups.some((g) => g.id === cur.id)) return;
    const params = new URLSearchParams(searchParams);
    params.delete('sel');
    params.delete('tab');
    setSearchParams(params, { replace: true });
  }, [loading, groups, searchParams, setSearchParams]);

  /** Close the add dialog. Its fields live in the dialog, so closing it is the reset. */
  const closeAdd = () => {
    setAdding(false);
    setAddGroupId(null);
  };

  /** Open the add-node dialog filed into `groupId` (`null` = ungrouped / top level). One opener for
   *  every entry point — the group-detail pane's button used to skip the folder and drop the node
   *  at top level, which is what a second copy of two setState calls buys you. */
  const openAddNode = (groupId: string | null) => {
    setAddGroupId(groupId);
    setAdding(true);
  };

  // Direct moves (drag-drop): assign immediately and refresh.
  //
  // 🚨 **A list, and the same bulk request the dialogs send** (ADR-124 Inc.4). This took one id
  // and called the single-node endpoint, so a drag of three checked nodes moved one — and the
  // two paths out of this screen disagreed about what moving a node even is. One request now,
  // whether the operator dragged one row or thirty.
  //
  // ⚠️ **A drop has no dialog to hold a partial result in.** `MoveNodeModal` stays open on
  // `moved < requested` so the operator reads it; a drop has nothing open, so the page says it.
  // Reporting a short move as success is the failure this endpoint returns two numbers for.
  const moveNodes = (
    nodeIds: readonly string[],
    groupId: string | null,
    placement?: { before?: string; after?: string },
  ) =>
    api
      .moveNodes([...nodeIds], groupId, placement)
      .then(async (r) => {
        // Only when the drag actually took the batch. Grabbing a row outside it leaves it
        // alone, which is what the right-click menu does with the same row (`nodeMoveItems`).
        if (nodeIds.some((id) => checked.has(id))) clearChecked();
        // 🚨 **After the refresh, never before.** `reload` opens with `setError(null)`, so a
        // partial reported first is wiped in the same tick and the drop reads as a clean
        // success — which is the exact failure the two numbers exist to prevent.
        await reload();
        if (r.moved < r.requested) setError(t('err.movePartial', { ...r }));
      })
      .catch((e: unknown) => setError(errMsg(e, t('err.moveNode'))));

  // Nest a group under another (or null = top level), appending it to the end of the destination —
  // the placement endpoint cycle-guards the move and assigns an append order in one call.
  const moveGroup = (groupId: string, parentGroupId: string | null) =>
    api
      .placeNodeGroup(groupId, { parent_id: parentGroupId })
      .then(reload)
      .catch((e: unknown) => setError(errMsg(e, t('err.moveGroup'))));

  // Drag-reorder (before/after a sibling row): place the item relative to a neighbour and refresh.
  // ⚠️ Since ADR-162 the neighbour may be a NODE — folders and nodes under one parent are one
  // ordered list — so `dest.before`/`after` carry whichever row the cursor was on.
  // ⚠️ There is no node twin of this any more — a node drop, at an edge or not, goes through
  // `moveNodes` above (ADR-124 増分 8), so "what happens when you move a node" has one answer.
  const reorderGroup = (
    groupId: string,
    dest: { parentId: string | null; before?: string; after?: string },
  ) =>
    api
      .placeNodeGroup(groupId, { parent_id: dest.parentId, before: dest.before, after: dest.after })
      .then(reload)
      .catch((e: unknown) => setError(errMsg(e, t('err.reorderGroup'))));

  // Arrange one folder's direct children in name order (ADR-130). One request, not one per child:
  // the browser does not hold a folder's whole membership (it is fetched lazily and capped
  // server-side), and a per-child loop would be a partial write with nothing to read back when it
  // fails halfway — the same shape a multi-node drag takes, placing the whole batch in one request.
  const sortGroupChildren = (groupId: string, direction: 'asc' | 'desc') =>
    api
      .sortNodeGroupChildren(groupId, direction)
      .then(reload)
      .catch((e: unknown) => setError(errMsg(e, t('err.sortChildren'))));

  // Right-click → pin or unpin (ADR-146). The mark changes at once; the store puts it back and this
  // says why when the server refuses (the 500-pin cap, a node deleted meanwhile).
  const togglePin = (target: { kind: 'node' | 'group'; id: string }) => {
    const store = usePinsStore.getState();
    const call =
      target.kind === 'node'
        ? store.setNodePinned(target.id, !store.nodeIds.has(target.id))
        : store.setGroupPinned(target.id, !store.groupIds.has(target.id));
    call.catch((e: unknown) => setError(errMsg(e, t('tree.pinFailed'))));
  };

  // Header stats come from the server fleet summary (whole fleet, not the lazily-loaded subset).
  // `null` until `/fleet/summary` has answered. It used to fall back to `treeNodes.length` — the
  // handful of members the viewport happened to load — and presented that as the fleet total.
  const nodeCount = fleetSummary?.total ?? null;
  const attention = fleetSummary
    ? fleetSummary.states.warning + fleetSummary.states.critical + fleetSummary.states.unreachable
    : 0;
  const anyGroupTruncated = members.anyTruncated;
  // Filter mode bypasses groups, so the group-truncation notice above never fires for it —
  // a fleet with more matches than the cap would otherwise show a silently short list. Counts the
  // SEARCH PAGE, not `treeNodes`: the latter also carries the revealed folders' members, which would
  // trip the cap notice for a term that never came near it.
  const filterTruncated = truncationNotice(
    search.truncated,
    search.nodes.length,
    FILTER_SEARCH_LIMIT,
  );
  // The selected node's summary, if it's among the loaded members (for the Move action). The detail
  // pane itself renders from the id, so it still works for a selection whose group isn't loaded.
  //
  // 🚨 **Indexed, not scanned** (ADR-133). These were three `find`s over `treeNodes`/`groups`, and
  // the comment below used to explain why memoizing them was pointless: `selected` is re-parsed into
  // a fresh object every render, so a memo keyed on it never hits. That reasoning was right and the
  // conclusion was wrong — what to memoize is the INDEX, whose key is the array, not the lookup.
  // `NodesPage` re-renders on every SSE flush (up to ten a second), and at ten thousand loaded
  // members that was three linear scans per flush for three single-row answers.
  const nodeById = useMemo(() => new Map(treeNodes.map((n) => [n.id, n])), [treeNodes]);
  const groupById = useMemo(() => new Map(groups.map((g) => [g.id, g])), [groups]);
  const selectedGroup = selected?.kind === 'group' ? groupById.get(selected.id) ?? null : null;
  // What the pane-head ＋ acts on: the selected group, a selected node's folder, else top level.
  const addTarget = addMenuTarget(selected, groupById, nodeById);

  return (
    <div className={selected ? 'page-fill nodes-detail-active' : 'page-fill'}>
      <PageHeader
        title={t('nav:nodes.all')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.all') }]}
        note={
          <>
            {nodeCount !== null && (
              <>
                {nodeCount} {t('common:noun.node', { count: nodeCount })} ·{' '}
              </>
            )}
            {t('inventory.groupCount', { count: groups.length })}
            {attention > 0 && (
              <>
                {' '}
                ·{' '}
                {/* The count is the way in (ADR-163). It already names the set the preset selects,
                    so making it press the preset costs no new control and no new vocabulary — and
                    it is the one place the number and the filter can be seen agreeing. Shares
                    `pressAttention` with the toggle in the filter row, so there is one behaviour to
                    reason about rather than two that look alike. */}
                <button
                  type="button"
                  className="nodes-attention"
                  aria-pressed={attentionOnly}
                  title={t('inventory.needAttentionOnlyHint')}
                  onClick={() => pressAttention(true)}
                >
                  {t('inventory.needAttention', { count: attention })}
                </button>
              </>
            )}
          </>
        }
        actions={
          canConfig && (
            // Deliberately top level, not the tree selection: this button survives the inventory
            // pane being collapsed to a rail, where the operator cannot see what is selected. The
            // dialog's Group select is where a different folder gets chosen.
            <Button variant="primary" onClick={() => openAddNode(null)}>
              {t('add.node')}
            </Button>
          )
        }
      />

      {error && <p className="form-error">{error}</p>}
      {/* 🚨 Said, with a way to ask again. A failed search used to come back as an empty list, and an
          empty filtered tree draws nothing — so under "Needs attention" a failed read was a blank
          pane, which reads as "nothing needs attention". */}
      {filtering && search.failed && (
        <p className="form-error" role="alert">
          {t('err.searchNodes')}{' '}
          <Button onClick={search.refetch}>{t('common:actions.retry')}</Button>
        </p>
      )}
      {anyGroupTruncated && (
        <p className="muted nodes-truncated">{t('inventory.groupTruncated')}</p>
      )}
      {filterTruncated === 'page' && (
        <p className="muted nodes-truncated">
          {t('inventory.filterTruncated', { count: FILTER_SEARCH_LIMIT })}
        </p>
      )}
      {filterTruncated === 'scan' && (
        <p className="muted nodes-truncated">{t('inventory.filterScanTruncated')}</p>
      )}
      {members.revealTruncated && (
        <p className="muted nodes-truncated">{t('inventory.revealTruncated')}</p>
      )}

      <div
        ref={setSplitEl}
        className={`nodes-split${selected ? ' has-sel' : ''}${railed ? ' inv-collapsed' : ''}`}
        // A custom property rather than an inline `gridTemplateColumns`: the mobile and ≤860px
        // rules in NodesPage.css re-declare the columns, and an inline declaration would beat them.
        style={{ ['--nodes-tree-w' as string]: `${treeWidth}px` }}
      >
        {railed ? (
          <div className="nodes-pane nodes-rail">
            <button
              type="button"
              className="nodes-rail-btn"
              onClick={toggleNodesPane}
              title={t('inventory.showTree')}
              aria-label={t('inventory.showTree')}
            >
              »
            </button>
          </div>
        ) : (
          <div className="nodes-pane">
          <div className="nodes-pane-head">
            <span className="nodes-pane-title">{t('nav:groups.inventory')}</span>
            <div className="nodes-pane-tools">
              {!isMobileView && (
                <button
                  type="button"
                  className="nodes-pane-collapse"
                  onClick={toggleNodesPane}
                  title={t('inventory.hideTree')}
                  aria-label={t('inventory.hideTree')}
                >
                  «
                </button>
              )}
              {canConfig && (
                // Both entries target the same folder — the tree selection, or top level. Adding a
                // node used to be right-click-only, which touch devices never fire.
                <ActionMenu
                  label={t('addMenu.label')}
                  align="end"
                  items={[
                    {
                      key: 'node',
                      label: t(addTarget.addNodeKey, { name: addTarget.groupName ?? '' }),
                      onSelect: () => openAddNode(addTarget.groupId),
                    },
                    {
                      key: 'group',
                      label: t(addTarget.addGroupKey, { name: addTarget.groupName ?? '' }),
                      onSelect: () => setGroupModal({ mode: 'add', parentId: addTarget.groupId }),
                    },
                  ]}
                  trigger={(p) => (
                    <Button
                      {...p}
                      variant="outline"
                      className="nodes-pane-add"
                      title={t('addMenu.trigger')}
                      aria-label={t('addMenu.trigger')}
                    >
                      ＋
                    </Button>
                  )}
                />
              )}
              {/* The clear affordance is the box's own, and it clears the box and nothing else.
                  clearAllFilters would take the state / kind / pool controls with it — three
                  filters the operator did not ask to drop. ClearFilters in the action row is the
                  control that means all of them. The ✕ empties the draft; the settle (which is
                  immediate for an empty box) removes `q` from the URL and leaves the rest. */}
              <SearchField
                boxClassName="nodes-pane-search"
                value={filter}
                onChange={(e) => term.setDraft(e.target.value)}
                onClear={() => term.setDraft('')}
                placeholder={t('inventory.searchPlaceholder')}
              />
            </div>
          </div>
          {/* The tree has no header row to hang a filter row under, so the controls carry their
              own names (ADR-053 Inc.6 decision E). All three are multi-select and reach the server
              as comma-joined sets.
              ⚠️ **A sibling of `.nodes-pane-head`, not a child of it.** That header is a
              single-line flex row with a fixed 38px height, so a control placed inside it shares
              the line with the title, the buttons and the search box — and the filter bar shipped
              squeezed to nothing there. The same mistake as putting a filter in Discovery's 28px
              select column: the container's size was never checked. */}
          <div className="nodes-pane-filters">
            <FilterButton
              columns={filterCols}
              filters={inventoryFilters}
              onOpen={() => setFilterSheet(true)}
            />
            {/* Pinned only (ADR-146), right of Filter. The same button look as Filter, pressed the
                same way, because it is the same kind of control: it narrows this tree. Not drawn
                until the pins have loaded — a core without the endpoint gets no button. */}
            {pinsReady && (
              <button
                type="button"
                className={pinnedOnly ? 'mfilt-btn nodes-pinned-only on' : 'mfilt-btn nodes-pinned-only'}
                aria-pressed={pinnedOnly}
                title={t('inventory.pinnedOnlyHint')}
                onClick={() => setNodeTreePinnedOnly(!pinnedOnly)}
              >
                <PinIcon />
                {t('inventory.pinnedOnly')}
              </button>
            )}
            {/* Folders with nodes only (ADR-159), right of Pinned only. The same button, pressed
                the same way, for the same reason: it narrows this tree. Always drawn — it reads
                the per-folder counts the tree already has, so there is no endpoint to be missing. */}
            <button
              type="button"
              className={withNodesOnly ? 'mfilt-btn on' : 'mfilt-btn'}
              aria-pressed={withNodesOnly}
              title={t('inventory.withNodesOnlyHint')}
              onClick={() => setNodeTreeWithNodesOnly(!withNodesOnly)}
            >
              {t('inventory.withNodesOnly')}
            </button>
            {/* Needs attention (ADR-163), right of With nodes. The same button for the third time,
                because it is the same kind of control — but unlike the two beside it this one holds
                nothing: it is a preset over the `state` filter below, so its pressed look is read
                back out of that filter rather than out of a switch. Press it and the State trigger
                lights up too, which is what says *what* it did. */}
            <button
              type="button"
              className={attentionOnly ? 'mfilt-btn on' : 'mfilt-btn'}
              aria-pressed={attentionOnly}
              title={t('inventory.needAttentionOnlyHint')}
              onClick={() => pressAttention(false)}
            >
              {t('inventory.needAttentionOnly')}
            </button>
            <FilterBar
              columns={filterCols}
              labels={filterLabels}
              filters={inventoryFilters}
              onChange={setInventoryFilters}
            />
            {/* The search box is a filter the operator can see but the row does not own, so it is
                counted here as `extra` and cleared by the same handler — an operator who presses
                "clear all filters" and is still looking at a narrowed tree has been told something
                untrue. */}
            <ClearFilters
              columns={filterCols}
              filters={inventoryFilters}
              extraActive={filter.trim() !== '' || pinnedOnly || withNodesOnly}
              onClear={clearAllFilters}
            />
            {filterSheet && (
              <MobileFilterSheet
                columns={filterCols}
                labels={filterLabels}
                filters={inventoryFilters}
                onChange={setInventoryFilters}
                onClose={() => setFilterSheet(false)}
              />
            )}
          </div>
          <NodeTree
            groups={groups}
            nodes={liveTreeNodes}
            groupCounts={groupCounts}
            // Asked for, not yet answered — which is a different statement from "every folder is
            // empty" and is what lets the members start arriving while the rollup is in flight
            // (ADR-133). `groupCounts` stays `{}` in that window so `GroupDetail` and
            // `groupDeletionImpact` keep the shape they expect; the flag is what the tree reads.
            countsPending={groupSummary === null}
            loadedGroups={members.loadedGroups}
            revealedGroups={members.revealedGroups}
            failedGroups={members.failedGroups}
            onRetryGroup={members.retry}
            onPendingGroupsChange={setVisibleGroupKeys}
            canEdit={canConfig}
            loading={loading || (filtering && search.loading)}
            showToolbar={false}
            selected={selected}
            // 🚨 The APPLIED (debounced) term, not the raw box (ADR-125). `flattenTree` walks every
            // group's subtree while narrowing, so passing the raw value ran that whole walk on
            // every keystroke while the search it belongs to was already debounced to 200ms.
            // ⚠️ `filtering` above deliberately stays on the RAW value: it decides whether the tree
            // is in filter mode at all, and lagging it by 200ms would leave the browse-mode fetch
            // running over a tree the operator has already started narrowing.
            filter={search.appliedTerm}
            // The tree cannot see the state / kind / pool controls — those run server-side — so it
            // has to be told, or it does not know it is filtering and hides nothing.
            narrowed={serverNarrowed}
            narrowKey={inventoryKey(inventoryFilters)}
            // The RAW question, which `filter` above lags — by the whole first render after a reload,
            // while `?q=` is already in hand. The tree keeps the folders pressed under a search until
            // both say the search is gone (ADR-154 increment 2).
            searchRequested={filtering}
            onSelectNode={(n) => select({ kind: 'node', id: n.id })}
            onSelectGroup={(g) => select({ kind: 'group', id: g.id })}
            onSelectNone={() => select(null)}
            onOpenNode={(n) => navigate(`/nodes/${n.id}`)}
            onAddGroup={(pid) => setGroupModal({ mode: 'add', parentId: pid })}
            onEditGroup={(g) => setGroupModal({ mode: 'edit', group: g, parentId: g.parent_id ?? null })}
            onDeleteGroup={(g) => setDeletingGroup(g)}
            onEditNode={canConfig ? (n) => setEditingNode(n) : undefined}
            onAddNode={canConfig ? openAddNode : undefined}
            onDeleteNode={canConfig ? (n) => setDeletingNode(n) : undefined}
            onDeleteChecked={canConfig ? () => setDeletingNodes([...checked.values()]) : undefined}
            checked={checked}
            anchorId={anchorId}
            // ⚠️ Ctrl / Shift marking is offered for any of the three permissions, not just
            // `canConfig` — since 増分 11 an operator who may only suppress still has batch verbs
            // to reach, and gating the *selection* on the strictest of them would hide them all.
            onPollNodes={canConfig ? pollNodes : undefined}
            onCheckedChange={
              canConfig || canMaintenance || canAck
                ? (next, anchor) => {
                    setChecked(next);
                    setAnchorId(anchor);
                  }
                : undefined
            }
            onRequestMoveNode={(n) => setMoving([n])}
            onMoveChecked={canConfig ? () => setMoving([...checked.values()]) : undefined}
            onMoveCheckedByPrefix={
              canConfig ? () => setMovingByPrefix([...checked.values()]) : undefined
            }
            onMoveNodeByPrefix={canConfig ? (n) => setMovingByPrefix([n]) : undefined}
            onTagChecked={canConfig ? () => setTaggingNodes([...checked.values()]) : undefined}
            onMoveNodes={moveNodes}
            onMoveGroup={moveGroup}
            onReorderGroup={reorderGroup}
            onSortGroupChildren={sortGroupChildren}
            suppression={suppression}
            suppressionRows={suppressionRows}
            onRelease={canMaintenance || canAck ? release : undefined}
            onSetMaintenance={canMaintenance ? setMaintenance : undefined}
            onSetMute={canAck ? setMute : undefined}
            pools={pools}
            onSetPool={canConfig ? setPool : undefined}
            onRunDiscovery={
              canConfig
                ? (g) => navigate(`/nodes/discovery?group=${encodeURIComponent(g.id)}`)
                : undefined
            }
            pins={pins}
            pinnedOnly={pinnedOnly}
            withNodesOnly={withNodesOnly}
            keepGroups={createdGroups}
            // Not permission-gated: pinning is the account's own navigation (ADR-146).
            onTogglePin={pinsReady ? togglePin : undefined}
          />
          {/* The working set's own row (ADR-124 決定 3, moved below the tree by 増分 5). It appears
              only once something is checked, so it costs nothing until it is needed — and it
              carries the gesture in **visible text**, because Ctrl / Shift is written nowhere else
              on the screen and a `title=` is unreadable on touch (ADR-055 R4).

              ⚠️ **Still never inside `.nodes-pane-head`** — that is a 38px single-line flex and has
              already squeezed one control to nothing. But **after** the tree, not before it:
              `.ntree-body` is `flex: 1`, so a `flex: none` sibling appearing above it dropped the
              scroller's top edge ~60px and every visible row translated down two rows on the first
              Ctrl click. Below it, neither the scroller's top edge nor its `scrollTop` moves, so no
              row moves at all — the bottom two rows are covered instead (増分 5 決定 A).
              🚨 **Do not "fix" that by writing `scrollTop`** the way
              `NodeDetail/InterfacesTab.tsx`'s `keepSelectedInView` does. Scrolling the tree for the
              operator is precisely what 増分 5 exists to stop. */}
          {/* ⚠️ The bar itself opens for **any** of the three permissions, and each control
              inside it is gated on its own (ADR-056). It was `canConfig` alone, so an operator
              who may silence a fleet but not reshape it had no bar at all — and since 増分 11 the
              bar is where the batch suppression lives. */}
          {(canConfig || canMaintenance || canAck) && checked.size > 0 && (
            <div className="nodes-selbar">
              <span className="nodes-selbar-count">
                {t('select.count', { count: checked.size })}
              </span>
              {canConfig && (
                <Button variant="outline" onClick={() => setMoving([...checked.values()])}>
                  {t('select.move')}
                </Button>
              )}
              {canMoveByPrefix(groups, canConfig) && (
                <Button variant="outline" onClick={() => setMovingByPrefix([...checked.values()])}>
                  {t('select.moveByPrefix')}
                </Button>
              )}
              {/* 🚨 The bar and the context menu must offer the same verbs (ADR-124 増分 9). Tag
                  was reachable only by right-clicking a checked row, so an operator working from
                  the bar had no way to know bulk tagging exists at all — the shape ADR-055 R6
                  is about, with the feature present rather than absent. */}
              {canConfig && (
                <Button variant="outline" onClick={() => setTaggingNodes([...checked.values()])}>
                  {t('select.tag')}
                </Button>
              )}
              {/* The verbs that are not the common two. They go in a menu rather than as buttons
                  because `.nodes-selbar` is one wrapping flex line above the tree, and every line
                  it wraps to covers another row of the inventory (増分 5 決定 A). */}
              {selectionMenuItems.length > 0 && (
                <ActionMenu
                  label={t('select.more')}
                  items={selectionMenuItems}
                  trigger={(p) => (
                    <Button variant="outline" {...p}>
                      {t('select.more')}
                    </Button>
                  )}
                />
              )}
              {canConfig && (
                <Button variant="outline" onClick={() => setDeletingNodes([...checked.values()])}>
                  {t('select.delete')}
                </Button>
              )}
              <Button variant="outline" onClick={clearChecked}>
                {t('select.clear')}
              </Button>
              <span className="nodes-selbar-hint">{t('select.hint')}</span>
            </div>
          )}
          {/* A manual poll's only feedback, below the tree like the selection bar so it cannot
              move a row (増分 5 決定 A). Outside the bar's own gate: a poll started from a row's
              context menu has to report itself with nothing selected. */}
          {pollMsg && (
            <p className={`nodes-pollmsg${pollMsg.tone === 'error' ? ' err' : ''}`}>
              {pollMsg.text}
            </p>
          )}
          </div>
        )}

        {/* The seam between the panes, as a real control: focusable, announced, arrow-key operable
            and resettable by double-click. How the screen is divided between "what is there" and
            "what is wrong with it" is an operator decision, not a constant (ADR-074).
            `role="slider"` with explicit bounds, matching the Interfaces dock — `separator` is the
            more literal role for a splitter, but two handles already ship as sliders and a third
            spelling would be the surprise. Not rendered while railed (nothing to proportion) or on
            mobile (one pane at a time, ADR-027). */}
        {!railed && !isMobileView && (
          <div
            className="nodes-split-handle"
            role="slider"
            tabIndex={0}
            aria-label={t('inventory.resizePane')}
            aria-orientation="horizontal"
            aria-valuenow={treeWidth}
            aria-valuemin={TREE_MIN_PX}
            aria-valuemax={maxTreeWidth(splitPx)}
            title={t('inventory.resizePane')}
            onPointerDown={onSplitDown}
            onPointerMove={onSplitMove}
            onPointerUp={endSplitDrag}
            onPointerCancel={endSplitDrag}
            onKeyDown={onSplitKey}
            onDoubleClick={() => setStoredWidth(null)}
          >
            <span className="nodes-split-grip" aria-hidden="true" />
          </div>
        )}

        <div className="nodes-pane nodes-detail-pane">
          {/* Mobile-only back control (ADR-027 pane switcher) — returns to the full-screen tree by
              clearing the ?sel= selection. Hidden on desktop via CSS. */}
          {selected && (
            <button
              type="button"
              className="nodes-detail-back"
              onClick={() => select(null)}
            >
              ‹ {t('inventory.backToList')}
            </button>
          )}
          {selected?.kind === 'node' ? (
            // Render from the selection id so a node whose group isn't loaded still shows detail
            // (the detail pane fetches the node itself). Move needs the loaded summary — guarded.
            <NodeDetail
              key={`${selected.id}:${detailNonce}`}
              nodeId={selected.id}
              variant="inline"
              canEdit={canConfig}
              tab={tab}
              onTabChange={setTab}
              groups={groups}
              nodes={treeNodes}
              // The pane hands over the node it loaded by id — see `NodeDetail`'s `onMove`.
              onMove={(n) => setMoving([n])}
              onOpenDetail={() => navigate(`/nodes/${selected.id}`)}
              // The breadcrumb opens a folder the same way its tree row does (ADR-142), so Escape
              // and the dropped `tab` behave exactly as they do after a row click.
              onOpenGroup={(id) => select({ kind: 'group', id })}
              // An edit made in this pane changes the row the tree is drawing beside it — its name
              // and its pool are both editable from that dialog (ADR-135).
              onChanged={() => void reload()}
            />
          ) : selectedGroup ? (
            <GroupDetail
              group={selectedGroup}
              groups={groups}
              // The member cache, not `treeNodes`: under a state / kind / pool filter `treeNodes` is
              // the server's search page alone, so a folder whose nodes all missed the filter read as
              // empty. The selected folder's direct members load whether or not a filter is on
              // (`useLazyGroupMembers`), so this is its whole membership once `membersFetch` says so.
              nodes={members.nodes}
              groupCounts={groupCounts}
              membersFetch={memberFetchState(
                selectedGroup.id,
                members.loadedGroups,
                members.failedGroups,
              )}
              onRetryMembers={() => members.retry(selectedGroup.id)}
              canEdit={canConfig}
              onEditGroup={(g) => setGroupModal({ mode: 'edit', group: g, parentId: g.parent_id ?? null })}
              onAddNode={() => openAddNode(selectedGroup.id)}
              onOpenGroup={(id) => select({ kind: 'group', id })}
              onOpenNode={(id) => select({ kind: 'node', id })}
              onPinError={(e) => setError(errMsg(e, t('tree.pinFailed')))}
            />
          ) : (
            <div className="nd-empty">
              {loading ? t('inventory.loadingInventory') : t('inventory.selectPrompt')}
            </div>
          )}
        </div>
      </div>

      {adding && (
        <AddNodeModal
          groups={groups}
          groupId={addGroupId}
          onClose={closeAdd}
          onCreated={() => {
            closeAdd();
            void reload();
          }}
        />
      )}

      {groupModal && (
        <GroupModal
          state={groupModal}
          groups={groups}
          onClose={() => setGroupModal(null)}
          onSaved={(groupId) => {
            // A folder just created is empty, so "Folders with nodes only" would drop it the
            // moment the reload below brings it in (ADR-159). Recorded on every create, switch on
            // or off: pressing the switch afterwards should not make the new folder vanish either.
            if (groupModal.mode === 'add') {
              setCreatedGroups((prev) => new Set(prev).add(groupId));
            }
            setGroupModal(null);
            void reload();
          }}
        />
      )}

      {deletingGroup && (
        <ConfirmDeleteModal
          title={t('group.delete')}
          onConfirm={() => api.deleteNodeGroup(deletingGroup.id)}
          errorFallback={t('err.deleteGroup')}
          onClose={() => setDeletingGroup(null)}
          onDone={() => {
            setDeletingGroup(null);
            void reload();
          }}
        >
          <Trans
            t={t}
            i18nKey="deleteGroup.confirm"
            values={{
              name: deletingGroup.name,
              impact: groupDeletionImpact(groups, groupSummary ? groupCounts : null, deletingGroup, t),
            }}
            components={{ b: <strong /> }}
          />
        </ConfirmDeleteModal>
      )}

      {deletingNode && (
        <DeleteNodeModal
          nodeId={deletingNode.id}
          name={deletingNode.name}
          onClose={() => setDeletingNode(null)}
          onDeleted={() => {
            // If the deleted node was the open selection, clear the right pane.
            if (selected?.kind === 'node' && selected.id === deletingNode.id) select(null);
            setDeletingNode(null);
            void reload();
          }}
        />
      )}

      {deletingNodes && (
        <DeleteNodesModal
          targets={deletingNodes}
          onClose={() => setDeletingNodes(null)}
          onDeleted={() => {
            // What went is gone even when some did not: the pane, the working set and the tree follow
            // it either way, and the dialog itself says whether everything went.
            if (selected?.kind === 'node' && deletingNodes.some((n) => n.id === selected.id)) {
              select(null);
            }
            clearChecked();
            void reload();
          }}
        />
      )}

      {editingNode && (
        <EditNodeModalById
          nodeId={editingNode.id}
          name={editingNode.name}
          onClose={() => setEditingNode(null)}
          onDone={() => {
            if (selected?.kind === 'node' && selected.id === editingNode.id) {
              setDetailNonce((v) => v + 1);
            }
            setEditingNode(null);
            void reload();
          }}
        />
      )}

      {moving && (
        <MoveNodeModal
          targets={moving}
          groups={groups}
          onClose={() => setMoving(null)}
          // Refresh only — the dialog stays open on a partial result so the operator reads it.
          onMoved={() => {
            clearChecked();
            void reload();
          }}
        />
      )}

      {movingByPrefix && (
        <MoveByPrefixModal
          targets={movingByPrefix}
          groups={groups}
          onClose={() => setMovingByPrefix(null)}
          onMoved={() => {
            clearChecked();
            void reload();
          }}
        />
      )}

      {taggingNodes && (
        <BulkTagModal
          targets={taggingNodes}
          onClose={() => setTaggingNodes(null)}
          onDone={() => {
            setTaggingNodes(null);
            clearChecked();
            // The tree does not draw tags, but the right-hand detail pane does — and it is keyed
            // by `detailNonce`, so a reload is what makes a freshly tagged selected node show them.
            setDetailNonce((v) => v + 1);
            void reload();
          }}
        />
      )}

      {maintenanceTarget && (
        <AddMaintenanceWindowModal
          groups={groups}
          initialScope={maintenanceTarget}
          onClose={() => setMaintenanceTarget(null)}
          onSaved={() => {
            if (maintenanceTarget.kind === 'nodes') clearChecked();
            void reloadSuppression();
          }}
        />
      )}
      {muteTarget && (
        <AddMuteModal
          groups={groups}
          initialScope={muteTarget}
          onClose={() => setMuteTarget(null)}
          onSaved={() => {
            if (muteTarget.kind === 'nodes') clearChecked();
            void reloadSuppression();
          }}
        />
      )}
      {poolTarget && (
        <SetPoolModal
          target={poolTarget}
          // ⚠️ A set has a shared pool only when every node in it agrees; the first node's is not
          // the batch's. `inheritedPool` is left off for a set for the same reason — the members
          // can sit under different folders, so there is no one value to show as the fallback.
          currentPool={
            poolTarget.kind === 'nodes'
              ? sharedOwnPool(poolTarget.nodes)
              : poolTarget.kind === 'group'
                ? (groups.find((g) => g.id === poolTarget.id)?.pool ?? null)
                : (treeNodes.find((n) => n.id === poolTarget.id)?.pool ?? null)
          }
          inheritedPool={
            poolTarget.kind === 'nodes'
              ? undefined
              : inheritedGroupPool(
                  groups,
                  poolTarget.kind === 'group'
                    ? (groups.find((g) => g.id === poolTarget.id)?.parent_id ?? null)
                    : (treeNodes.find((n) => n.id === poolTarget.id)?.group_id ?? null),
                )
          }
          onClose={() => setPoolTarget(null)}
          onSaved={() => {
            if (poolTarget.kind === 'nodes') clearChecked();
            setPoolTarget(null);
            reloadPools();
            void reload();
          }}
        />
      )}
    </div>
  );
}

