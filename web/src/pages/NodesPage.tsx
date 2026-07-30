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
// matches under their groups — it never loads the fleet into the browser. The left pane is
// virtualized (only on-screen rows in the DOM, S13); node state stays live via the node-state SSE
// stream (`useNodeStates`) rather than a full refetch.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { api, errMsg } from '../services/api';
import { useAuthStore } from '../store';
import { usePrefsStore } from '../prefs';
import { useViewportMode } from '../lib/viewport';
import { GROUP_TYPES } from '../types/api';
import type {
  FleetGroupSummary,
  FleetSummary,
  GroupType,
  MaintenanceWindow,
  Mute,
  NodeGroup,
  NodeSummary,
  PoolOption,
} from '../types/api';
import {
  countsTotal,
  groupOptions,
  isSelfOrDescendant,
  type StateCounts,
} from '../lib/nodeTree';
import { useLazyGroupMembers } from './useLazyGroupMembers';
import { useNodeStates } from '../dashboard/useNodeStates';
import { buildSuppressionIndex, type SuppressionTarget } from '../lib/suppression';
import { inheritedGroupPool, isValidPoolName } from '../lib/pool';
import { parseSelection, selectionToParam } from '../lib/treeSelection';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { TextInput, Select, RequiredMark } from '../components/ui/Field';
import { AddNodeModal } from '../components/AddNodeModal/AddNodeModal';
import { NodeTree, type TreeSelection } from '../components/NodeTree/NodeTree';
import { NodeDetail, DeleteNodeModal } from '../components/NodeDetail/NodeDetail';
import { normalizeNodeDetailTab } from '../components/NodeDetail/tabs';
import { GroupDetail } from '../components/NodeDetail/GroupDetail';
import { MoveNodeModal } from '../components/MoveNodeModal/MoveNodeModal';
import { SetPoolModal } from '../components/SetPoolModal/SetPoolModal';
import { AddMaintenanceWindowModal } from '../components/suppression/AddMaintenanceWindowModal';
import { AddMuteModal } from '../components/suppression/AddMuteModal';
import './NodesPage.css';

/** Cap on filter-mode results — one server page of matches (the server's max). Beyond this the
 *  operator narrows the term; the fleet is never loaded into the browser. */
const FILTER_SEARCH_LIMIT = 500;
/** Debounce for the filter-mode server search so a fast typist fires one request, not one per key. */
const FILTER_DEBOUNCE_MS = 200;

/** Stable empty per-group counts (avoids a fresh `{}` each render churning the tree memo). */
const EMPTY_GROUP_COUNTS: Record<string, StateCounts> = {};

/** Add/edit a group: name, type, and parent (parent doubles as "move"). */
interface GroupModalState {
  mode: 'add' | 'edit';
  group?: NodeGroup;
  parentId: string | null;
}

export function NodesPage() {
  const { t } = useTranslation('nodes');
  const navigate = useNavigate();
  const authed = useAuthStore((s) => s.authed);
  const [groups, setGroups] = useState<NodeGroup[]>([]);
  // Server-side rollups: per-group direct counts drive the tree's group-row health bars (correct
  // over the whole fleet even before members load, A-1/A-3); the fleet summary drives the header
  // total + attention count without loading the inventory.
  const [groupSummary, setGroupSummary] = useState<FleetGroupSummary | null>(null);
  const [fleetSummary, setFleetSummary] = useState<FleetSummary | null>(null);
  // Filter mode full-loads the fleet once (then searches client-side); null = not yet loaded.
  const [allNodes, setAllNodes] = useState<NodeSummary[] | null>(null);
  const [allNodesLoading, setAllNodesLoading] = useState(false);
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

  // Suppression: active maintenance windows + mutes drive the per-row icons; the right-click
  // "Custom…" path opens a prefilled create modal (preset durations POST directly).
  const [windows, setWindows] = useState<MaintenanceWindow[]>([]);
  const [mutes, setMutes] = useState<Mute[]>([]);
  const [maintenanceTarget, setMaintenanceTarget] = useState<SuppressionTarget | null>(null);
  const [muteTarget, setMuteTarget] = useState<SuppressionTarget | null>(null);
  // Poll-pool assignment from the tree's right-click chips (ADR-009/020). `pools` feeds the chips;
  // `poolTarget` holds the target whose "Custom…" dialog is open.
  const [pools, setPools] = useState<PoolOption[]>([]);
  const [poolTarget, setPoolTarget] = useState<SuppressionTarget | null>(null);
  // Per-group direct counts (server rollup) → the tree's group-row health bars + the header stats.
  const groupCounts = groupSummary?.groups ?? EMPTY_GROUP_COUNTS;

  // Members load lazily, per group, only once that group's contents are on screen (A-3). The hook
  // owns that cache; this page only says what is currently worth having loaded.
  const filtering = filter.trim().length > 0;
  const members = useLazyGroupMembers({
    groups,
    collapsed,
    ready: !loading,
    browsing: !filtering,
    selectedGroupId: selected?.kind === 'group' ? selected.id : null,
  });
  const invalidateMembers = members.invalidate;

  // The nodes the tree renders: browse mode = the union of the lazily-loaded per-group members;
  // filter mode = the server search's single capped page.
  const treeNodes = useMemo(
    () => (filtering ? allNodes ?? [] : members.nodes),
    [filtering, allNodes, members.nodes],
  );

  // Overlay the live SSE node states (S14) so the tree's status dots update without re-fetching.
  const live = useNodeStates();
  const liveTreeNodes = useMemo(
    () =>
      treeNodes.map((n) => {
        const s = live.get(n.id);
        return s && s !== n.state ? { ...n, state: s } : n;
      }),
    [treeNodes, live],
  );

  const suppression = useMemo(
    () => buildSuppressionIndex(windows, mutes, groups, treeNodes),
    [windows, mutes, groups, treeNodes],
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
      // Invalidate the lazily-loaded members + filter cache so open groups re-fetch fresh.
      invalidateMembers();
      setAllNodes(null);
    } catch (e: unknown) {
      setError(errMsg(e, t('err.loadNodes')));
    } finally {
      setLoading(false);
    }
  }, [t, invalidateMembers]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // Filter mode: debounced SERVER-side name/address search — the tree searches the fleet without
  // loading it. Re-queries as the term changes; a stale response from an earlier keystroke is
  // dropped. Results are capped at one server page (keep typing to narrow).
  useEffect(() => {
    if (!filtering) return;
    const term = filter.trim();
    let cancelled = false;
    setAllNodesLoading(true);
    const h = setTimeout(() => {
      api
        .listNodesPage({ search: term, limit: FILTER_SEARCH_LIMIT })
        .then((page) => {
          if (!cancelled) setAllNodes(page.nodes);
        })
        .catch(() => {
          if (!cancelled) setAllNodes([]);
        })
        .finally(() => {
          if (!cancelled) setAllNodesLoading(false);
        });
    }, FILTER_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      clearTimeout(h);
    };
  }, [filtering, filter]);

  // Active maintenance windows + mutes for the per-row suppression icons. Refetched after any
  // maintenance/mute action from the tree so the icons update immediately (node `maintenance`
  // state catches up on the engine's next ~30s refresh).
  const reloadSuppression = useCallback(() => {
    api.listMaintenanceWindows().then(setWindows).catch(() => undefined);
    api.listMutes().then(setMutes).catch(() => undefined);
  }, []);

  useEffect(() => {
    reloadSuppression();
  }, [reloadSuppression]);

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
  // The selected node's summary, if it's among the loaded members (for the Move action). The detail
  // pane itself renders from the id, so it still works for a selection whose group isn't loaded.
  const selectedNode =
    selected?.kind === 'node' ? treeNodes.find((n) => n.id === selected.id) ?? null : null;
  const selectedGroup =
    selected?.kind === 'group' ? groups.find((g) => g.id === selected.id) ?? null : null;

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
          authed && (
            <Button
              variant="primary"
              onClick={() => {
                setAddGroupId(null);
                setAdding(true);
              }}
            >
              {t('add.node')}
            </Button>
          )
        }
      />

      {error && <p className="form-error">{error}</p>}
      {anyGroupTruncated && (
        <p className="muted nodes-truncated">{t('inventory.groupTruncated')}</p>
      )}

      <div
        className={`nodes-split${selected ? ' has-sel' : ''}${railed ? ' inv-collapsed' : ''}`}
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
              {authed && (
                <Button
                  variant="outline"
                  className="nodes-pane-add"
                  title={t('group.add')}
                  onClick={() => setGroupModal({ mode: 'add', parentId: null })}
                >
                  ＋
                </Button>
              )}
              <TextInput
                className="nodes-pane-search"
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                placeholder={t('inventory.searchPlaceholder')}
              />
            </div>
          </div>
          <NodeTree
            groups={groups}
            nodes={liveTreeNodes}
            groupCounts={groupCounts}
            loadedGroups={members.loadedGroups}
            canEdit={authed}
            loading={loading || (filtering && allNodesLoading)}
            showToolbar={false}
            selected={selected}
            filter={filter}
            onSelectNode={(n) => select({ kind: 'node', id: n.id })}
            onSelectGroup={(g) => select({ kind: 'group', id: g.id })}
            onOpenNode={(n) => navigate(`/nodes/${n.id}`)}
            onAddGroup={(pid) => setGroupModal({ mode: 'add', parentId: pid })}
            onEditGroup={(g) => setGroupModal({ mode: 'edit', group: g, parentId: g.parent_id })}
            onDeleteGroup={(g) => setDeletingGroup(g)}
            onAddNode={
              authed
                ? (gid) => {
                    setAddGroupId(gid);
                    setAdding(true);
                  }
                : undefined
            }
            onDeleteNode={authed ? (n) => setDeletingNode(n) : undefined}
            onRequestMoveNode={(n) => setMovingNode(n)}
            onMoveNode={moveNode}
            onMoveGroup={moveGroup}
            onReorderNode={reorderNode}
            onReorderGroup={reorderGroup}
            suppression={suppression}
            onSetMaintenance={authed ? setMaintenance : undefined}
            onSetMute={authed ? setMute : undefined}
            pools={pools}
            onSetPool={authed ? setPool : undefined}
          />
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
              key={selected.id}
              nodeId={selected.id}
              variant="inline"
              canEdit={authed}
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
              canEdit={authed}
              onEditGroup={(g) => setGroupModal({ mode: 'edit', group: g, parentId: g.parent_id })}
              onAddNode={() => setAdding(true)}
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

/** Add or edit a group (name + type + parent). Editing the parent moves the group; self and
 *  descendants are excluded from the parent options so a move can't create a cycle. */
function GroupModal({
  state,
  groups,
  onClose,
  onSaved,
}: {
  state: GroupModalState;
  groups: NodeGroup[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('nodes');
  const editing = state.mode === 'edit';
  const [name, setName] = useState(state.group?.name ?? '');
  const [type, setType] = useState<GroupType>(state.group?.group_type ?? 'generic');
  const [parent, setParent] = useState<string>(
    (editing ? state.group?.parent_id : state.parentId) ?? '',
  );
  /** The folder's own pool ('' ⇒ inherit from an ancestor, else the default pool). */
  const [pool, setPool] = useState(state.group?.pool ?? '');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const poolInvalid = !isValidPoolName(pool);
  // What this folder would inherit if its own pool is cleared. Preview only — the authority on what
  // actually polls a node is the server (`getNodeAssignment`), never this walk.
  const inherited = inheritedGroupPool(groups, parent || null);

  // For an edit, a group cannot be parented under itself or any of its descendants.
  const parentChoices = groupOptions(groups).filter(
    (o) => !(editing && state.group && isSelfOrDescendant(groups, state.group.id, o.id)),
  );

  const save = () => {
    setBusy(true);
    setError(null);
    // `pool` is always sent: '' clears it back to inherited (a JSON-absent field would mean
    // "unchanged" server-side and silently drop the edit).
    const body = {
      name: name.trim(),
      group_type: type,
      parent_id: parent || null,
      pool: pool.trim(),
    };
    const call = editing
      ? api.updateNodeGroup(state.group!.id, body)
      : api.createNodeGroup(body).then(() => undefined);
    call
      .then(onSaved)
      .catch((e: unknown) => {
        setError(errMsg(e, t('err.saveGroup')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={editing ? t('group.edit') : t('group.add')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={!name.trim() || busy || poolInvalid}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <label className="form-label">
          <span>
            {t('field.name')} <RequiredMark />
          </span>
          <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
        </label>
        <label className="form-label">
          {t('group.type')}
          <Select value={type} onChange={(e) => setType(e.target.value as GroupType)}>
            {GROUP_TYPES.map((gt) => (
              <option key={gt} value={gt}>
                {t(`groupType.${gt}`)}
              </option>
            ))}
          </Select>
        </label>
        <label className="form-label">
          {t('group.parentGroup')}
          <Select value={parent} onChange={(e) => setParent(e.target.value)}>
            <option value="">{t('group.topLevelOption')}</option>
            {parentChoices.map((o) => (
              <option key={o.id} value={o.id}>
                {o.label}
              </option>
            ))}
          </Select>
        </label>
        <label className="form-label">
          {t('group.pool')}
          <TextInput
            className="mono"
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            placeholder={inherited ? t('field.poolInheritPlaceholder', { pool: inherited }) : ''}
          />
          <span className={`form-hint${poolInvalid ? ' form-hint-error' : ''}`}>
            {poolInvalid ? t('field.poolInvalid') : t('group.poolHint')}
          </span>
        </label>
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
