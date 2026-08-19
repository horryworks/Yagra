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
// instantly at any fleet size. A group's member nodes are fetched only when it is open and visible
// (`/nodes/by-group`), streaming in per group; collapsed groups are never loaded. An active name
// filter runs a debounced SERVER-side search (`/nodes?search=`), capped at one page, and drops the
// matches under their groups — it never loads the fleet into the browser. A term matching a GROUP's
// name additionally loads that folder's whole subtree, since the server search matches nodes and
// knows nothing about groups, and the folder's contents are what the operator asked for. The left pane is
// virtualized (only on-screen rows in the DOM, S13); node state stays live via the node-state SSE
// stream (`useNodeStates`) rather than a full refetch.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import { usePrefsStore } from '../prefs';
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
import { countsTotal, mergeNodesById, type StateCounts } from '../lib/nodeTree';
import { overlayLiveStates, type LiveOverlay } from '../lib/liveOverlay';
import { FILTER_SEARCH_LIMIT, useFilterSearch } from './useFilterSearch';
import {
  inventoryColumns,
  inventoryFilterLabels,
  isInventoryFiltered,
  readInventoryFilters,
  truncationNotice,
  writeInventoryFilters,
} from './inventoryFilters';
import { FilterBar } from '../components/ui/FilterBar';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { defaultFilters, type FilterState } from '../lib/columnFilter';
import { useLazyGroupMembers } from './useLazyGroupMembers';
import { addMenuTarget } from './nodesAddMenu';
import { useNodeStates } from '../dashboard/useNodeStates';
import {
  buildSuppressionIndex,
  nextSuppressionExpiry,
  suppressionPanelRows,
  releasableRows,
  type ReleaseAction,
  type SuppressionTarget,
} from '../lib/suppression';
import { inheritedGroupPool } from '../lib/pool';
import { parseSelection, selectionToParam } from '../lib/treeSelection';
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
import { TextInput } from '../components/ui/Field';
import { AddNodeModal } from '../components/AddNodeModal/AddNodeModal';
import { GroupModal, type GroupModalState } from '../components/GroupModal/GroupModal';
import { NodeTree, type TreeSelection } from '../components/NodeTree/NodeTree';
import { NodeDetail, DeleteNodeModal } from '../components/NodeDetail/NodeDetail';
import { EditNodeModalById } from '../components/NodeDetail/EditNodeModal';
import { normalizeNodeDetailTab } from '../components/NodeDetail/tabs';
import { GroupDetail } from '../components/NodeDetail/GroupDetail';
import { MoveNodeModal } from '../components/MoveNodeModal/MoveNodeModal';
import { SetPoolModal } from '../components/SetPoolModal/SetPoolModal';
import { AddMaintenanceWindowModal } from '../components/suppression/AddMaintenanceWindowModal';
import { AddMuteModal } from '../components/suppression/AddMuteModal';
import './NodesPage.css';

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
  // The user's collapsed-group set (prefs) decides which groups are open → which members to load.
  const collapsed = usePrefsStore((s) => s.nodeTreeCollapsed);
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
  // (design-guidelines.md "画面状態の永続化"). The left-pane search box stays transient (local).
  const [searchParams, setSearchParams] = useSearchParams();
  const selected: TreeSelection = parseSelection(searchParams.get('sel'));
  const tabParam = searchParams.get('tab') ?? '';
  const tab = normalizeNodeDetailTab(tabParam);
  const [filter, setFilter] = useState('');
  // Pick a row → write the selection and reset to Overview (a fresh selection starts on Overview).
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

  // Escape clears the selection (ADR-073). Before this the split had no desktop way back to the
  // empty right pane at all: `select(null)` existed but its only caller was the mobile pane
  // switcher's back chevron, which `.nodes-detail-back { display: none }` hides on a desktop.
  //
  // The guard is shared rather than spelled here, because the dashboard's edit mode and the
  // Interfaces dock ask the same question and three copies would drift. It answers false while a
  // modal, a popover or the tree's context menu is open — those own the press — and false while the
  // operator is typing, so Escape still belongs to the pane's search box.
  useEffect(() => {
    if (!selected) return;
    const onKey = (e: KeyboardEvent) => {
      if (escapeClearsSelection(e)) select(null);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [selected, select]);

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
  const [movingNode, setMovingNode] = useState<NodeSummary | null>(null);
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
  const [maintenanceTarget, setMaintenanceTarget] = useState<SuppressionTarget | null>(null);
  const [muteTarget, setMuteTarget] = useState<SuppressionTarget | null>(null);
  // Poll-pool assignment from the tree's right-click chips (ADR-009/020). `pools` feeds the chips;
  // `poolTarget` holds the target whose "Custom…" dialog is open.
  const [pools, setPools] = useState<PoolOption[]>([]);
  const [poolTarget, setPoolTarget] = useState<SuppressionTarget | null>(null);
  // Per-group direct counts (server rollup) → the tree's group-row health bars + the header stats.
  const groupCounts = groupSummary?.groups ?? EMPTY_GROUP_COUNTS;

  // The three controls live in the URL beside the selection — they are the part someone shares
  // ("the URL monitors in the tokyo pool"), and a reload that dropped them would silently widen
  // the list back to the whole fleet. The text box stays local, as it always has: it is a scratch
  // typing surface, and the pane it filters is already addressable by these.
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

  // Members load lazily, per group, only once that group's contents are on screen (A-3). The hook
  // owns that cache; this page only says what is currently worth having loaded.
  // Any of the four puts the tree into filter mode. The state / kind / pool ones count even with
  // an empty box: "show me the URL monitors" is a whole question, and browsing the folder tree
  // while one is set would show every node and look like the control did nothing.
  const filtering = filter.trim().length > 0 || isInventoryFiltered(inventoryFilters);
  // Clearing everything is one handler, and it writes the URL exactly once. The search box is
  // component state and the three controls are URL state, so a "clear all" split across two
  // callbacks would be two `setSearchParams` calls from the same render snapshot — the second
  // restoring what the first cleared. That bug has already shipped once (`ClearFilters`' own doc).
  const clearAllFilters = useCallback(() => {
    setFilter('');
    setInventoryFilters(defaultFilters(filterCols));
  }, [filterCols, setInventoryFilters]);
  // Filter mode's server-side page — the nodes that matched. One capped page, never the fleet; the
  // folders a group-name match reveals arrive separately through the per-group member cache below.
  // `appliedTerm` is the debounced term the search was issued for, so the reveal loads in step with
  // the search rather than once per keystroke.
  const search = useFilterSearch(filter, inventoryFilters);
  const refetchSearch = search.refetch;
  const members = useLazyGroupMembers({
    groups,
    collapsed,
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
  const treeNodes = useMemo(
    () =>
      serverNarrowed
        ? search.nodes
        : filtering
          ? mergeNodesById(search.nodes, members.nodes)
          : members.nodes,
    [serverNarrowed, filtering, search.nodes, members.nodes],
  );

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
  const reload = useCallback(async () => {
    setError(null);
    try {
      const [g, gs, fs] = await Promise.all([
        api.listNodeGroups(),
        api.getFleetGroupSummary().catch(() => ({ groups: {} }) as FleetGroupSummary),
        api.getFleetSummary().catch(() => null),
      ]);
      setGroups(g);
      setGroupSummary(gs);
      setFleetSummary(fs);
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
  }, [t, invalidateMembers, refetchSearch]);

  useEffect(() => {
    void reload();
  }, [reload]);

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
  useEffect(() => {
    const at = nextSuppressionExpiry(windows, mutes, exemptions);
    if (at === null) return undefined;
    // A second past the expiry, so the row is gone from the server's answer rather than racing it.
    const h = setTimeout(reloadSuppression, at - Date.now() + 1000);
    return () => clearTimeout(h);
  }, [windows, mutes, exemptions, reloadSuppression]);

  // Right-click → maintenance/mute. A preset duration POSTs now → now+duration immediately; the
  // "Custom…" item (durationMs === null) opens the full create form prefilled with the scope.
  const scopeError = (e: unknown, fallback: string) => setError(errMsg(e, fallback));

  const setMaintenance = (target: SuppressionTarget, durationMs: number | null) => {
    if (durationMs === null) {
      setMaintenanceTarget(target);
      return;
    }
    const now = new Date();
    api
      .createMaintenanceWindow({
        name: t('maintenanceWindowName', { name: target.name }),
        scope_level: target.kind === 'group' ? 'group_id' : 'node',
        scope_id: target.id,
        starts_at: now.toISOString(),
        ends_at: new Date(now.getTime() + durationMs).toISOString(),
      })
      .then(reloadSuppression)
      .catch((e: unknown) => scopeError(e, t('err.setMaintenance')));
  };

  const setMute = (target: SuppressionTarget, durationMs: number | null) => {
    if (durationMs === null) {
      setMuteTarget(target);
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
  const setPool = (target: SuppressionTarget, pool: string | null) => {
    if (pool === null) {
      setPoolTarget(target);
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
  const moveNode = (nodeId: string, groupId: string | null) =>
    api.setNodeGroup(nodeId, groupId).then(reload).catch((e: unknown) =>
      setError(errMsg(e, t('err.moveNode'))),
    );

  // Nest a group under another (or null = top level), appending it to the end of the destination —
  // the placement endpoint cycle-guards the move and assigns an append order in one call.
  const moveGroup = (groupId: string, parentGroupId: string | null) =>
    api
      .placeNodeGroup(groupId, { parent_id: parentGroupId })
      .then(reload)
      .catch((e: unknown) => setError(errMsg(e, t('err.moveGroup'))));

  // Drag-reorder (before/after a sibling): place the item relative to a neighbour and refresh.
  const reorderNode = (
    nodeId: string,
    dest: { groupId: string | null; before?: string; after?: string },
  ) =>
    api
      .placeNode(nodeId, { group_id: dest.groupId, before: dest.before, after: dest.after })
      .then(reload)
      .catch((e: unknown) => setError(errMsg(e, t('err.reorderNode'))));

  const reorderGroup = (
    groupId: string,
    dest: { parentId: string | null; before?: string; after?: string },
  ) =>
    api
      .placeNodeGroup(groupId, { parent_id: dest.parentId, before: dest.before, after: dest.after })
      .then(reload)
      .catch((e: unknown) => setError(errMsg(e, t('err.reorderGroup'))));

  // Header stats come from the server fleet summary (whole fleet, not the lazily-loaded subset).
  const nodeCount = fleetSummary?.total ?? treeNodes.length;
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
  const selectedNode =
    selected?.kind === 'node' ? treeNodes.find((n) => n.id === selected.id) ?? null : null;
  const selectedGroup =
    selected?.kind === 'group' ? groups.find((g) => g.id === selected.id) ?? null : null;
  // What the pane-head ＋ acts on: the selected group, a selected node's folder, else top level.
  // Not memoized — `selected` is re-parsed into a fresh object every render, so a memo on it would
  // never hit, and this is two finds over the arrays the two lines above already scan.
  const addTarget = addMenuTarget(selected, groups, treeNodes);

  return (
    <div className={selected ? 'page-fill nodes-detail-active' : 'page-fill'}>
      <PageHeader
        title={t('nav:nodes.all')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.all') }]}
        note={
          <>
            {nodeCount} {t('common:noun.node', { count: nodeCount })} ·{' '}
            {t('inventory.groupCount', { count: groups.length })}
            {attention > 0 && (
              <>
                {' '}
                ·{' '}
                <span className="nodes-attention">
                  {t('inventory.needAttention', { count: attention })}
                </span>
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
              <TextInput
                className="nodes-pane-search"
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
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
              extraActive={filter.trim() !== ''}
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
            loadedGroups={members.loadedGroups}
            revealedGroups={members.revealedGroups}
            canEdit={canConfig}
            loading={loading || (filtering && search.loading)}
            showToolbar={false}
            selected={selected}
            filter={filter}
            // The tree cannot see the state / kind / pool controls — those run server-side — so it
            // has to be told, or it does not know it is filtering and hides nothing.
            narrowed={serverNarrowed}
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
            onRequestMoveNode={(n) => setMovingNode(n)}
            onMoveNode={moveNode}
            onMoveGroup={moveGroup}
            onReorderNode={reorderNode}
            onReorderGroup={reorderGroup}
            suppression={suppression}
            suppressionRows={suppressionRows}
            onRelease={canMaintenance || canAck ? release : undefined}
            onSetMaintenance={canMaintenance ? setMaintenance : undefined}
            onSetMute={canAck ? setMute : undefined}
            pools={pools}
            onSetPool={canConfig ? setPool : undefined}
          />
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
              onMove={() => selectedNode && setMovingNode(selectedNode)}
              onOpenDetail={() => navigate(`/nodes/${selected.id}`)}
            />
          ) : selectedGroup ? (
            <GroupDetail
              group={selectedGroup}
              groups={groups}
              nodes={treeNodes}
              canEdit={canConfig}
              onEditGroup={(g) => setGroupModal({ mode: 'edit', group: g, parentId: g.parent_id ?? null })}
              onAddNode={() => openAddNode(selectedGroup.id)}
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
          onSaved={() => {
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
              impact: groupDeletionImpact(groups, groupCounts, deletingGroup, t),
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

      {movingNode && (
        <MoveNodeModal
          node={movingNode}
          groups={groups}
          onClose={() => setMovingNode(null)}
          onMoved={() => {
            setMovingNode(null);
            void reload();
          }}
        />
      )}

      {maintenanceTarget && (
        <AddMaintenanceWindowModal
          groups={groups}
          initialScope={maintenanceTarget}
          onClose={() => setMaintenanceTarget(null)}
          onSaved={reloadSuppression}
        />
      )}
      {muteTarget && (
        <AddMuteModal
          groups={groups}
          initialScope={muteTarget}
          onClose={() => setMuteTarget(null)}
          onSaved={reloadSuppression}
        />
      )}
      {poolTarget && (
        <SetPoolModal
          target={poolTarget}
          currentPool={
            poolTarget.kind === 'group'
              ? (groups.find((g) => g.id === poolTarget.id)?.pool ?? null)
              : (treeNodes.find((n) => n.id === poolTarget.id)?.pool ?? null)
          }
          inheritedPool={inheritedGroupPool(
            groups,
            poolTarget.kind === 'group'
              ? (groups.find((g) => g.id === poolTarget.id)?.parent_id ?? null)
              : (treeNodes.find((n) => n.id === poolTarget.id)?.group_id ?? null),
          )}
          onClose={() => setPoolTarget(null)}
          onSaved={() => {
            setPoolTarget(null);
            reloadPools();
            void reload();
          }}
        />
      )}
    </div>
  );
}

/** One-line impact summary for deleting a group: how many direct subgroups and member nodes
 *  will be re-parented (nothing is deleted). The member count comes from the server per-group
 *  rollup (A-3) so it's correct without loading the group's members. Pluralised for readability. */
function groupDeletionImpact(
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
