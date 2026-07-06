// Nodes / All nodes — a two-pane split. The left pane is the inventory tree (groups → member
// nodes) with a per-group health rollup; selecting a row drives the right pane, which shows the
// chosen node's live detail (the shared <NodeDetail>, the same component as the /nodes/:id route)
// or a selected group's rollup. Triage and drill-in happen without leaving the inventory: pick a
// node, read its tabs, Poll now, move on. Add/rename/delete/move of groups and nodes runs through
// focused-edit modals (ManageConfig); 503 in skeleton mode is surfaced.
//
// Scale note: the tree needs the full group + node sets, so it loads all node pages up to a cap
// (NODE_CAP) and flags when an inventory is larger than that — virtualized lazy loading is the
// follow-up for very large fleets.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type {
  CredentialSummary,
  GroupType,
  MaintenanceWindow,
  Mute,
  NodeGroup,
  NodeSummary,
  ProfileSummary,
} from '../types/api';
import { groupOptions, isSelfOrDescendant, tallyStates } from '../lib/nodeTree';
import { buildSuppressionIndex, type SuppressionTarget } from '../lib/suppression';
import { parseSelection, selectionToParam } from '../lib/treeSelection';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, RequiredMark } from '../components/ui/Field';
import { NodeTree, type TreeSelection } from '../components/NodeTree/NodeTree';
import { NodeDetail, DeleteNodeModal } from '../components/NodeDetail/NodeDetail';
import { GroupDetail } from '../components/NodeDetail/GroupDetail';
import { MoveNodeModal } from '../components/MoveNodeModal/MoveNodeModal';
import { AddMaintenanceWindowModal } from '../components/suppression/AddMaintenanceWindowModal';
import { AddMuteModal } from '../components/suppression/AddMuteModal';
import './NodesPage.css';

const PAGE = 100;
/** Max nodes the tree loads (pages of PAGE). Beyond this we flag the inventory as truncated. */
const NODE_CAP = 5000;

const GROUP_TYPES: { value: GroupType; label: string }[] = [
  { value: 'site', label: 'Site' },
  { value: 'region', label: 'Region' },
  { value: 'device_type', label: 'Device type' },
  { value: 'service', label: 'Service' },
  { value: 'generic', label: 'Generic' },
];

const PROBLEM_STATES = new Set<NodeSummary['state']>(['warning', 'critical', 'unreachable']);

const TABS = ['overview', 'interfaces', 'collection', 'events'];

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

/** Add/edit a group: name, type, and parent (parent doubles as "move"). */
interface GroupModalState {
  mode: 'add' | 'edit';
  group?: NodeGroup;
  parentId: string | null;
}

export function NodesPage() {
  const navigate = useNavigate();
  const authed = useAuthStore((s) => s.authed);
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [groups, setGroups] = useState<NodeGroup[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // The right-pane selection and the inline detail tab live in the URL (`?sel=node:<id>&tab=…`)
  // so a browser reload restores the same pane instead of snapping back to the empty state
  // (design-guidelines.md "画面状態の永続化"). The left-pane search box stays transient (local).
  const [searchParams, setSearchParams] = useSearchParams();
  const selected: TreeSelection = parseSelection(searchParams.get('sel'));
  const tabParam = searchParams.get('tab') ?? '';
  const tab = TABS.includes(tabParam) ? tabParam : 'overview';
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
  /** Which kind of monitor to add: an SNMP/ICMP device, or a URL/HTTP(S) endpoint. */
  const [monType, setMonType] = useState<'device' | 'url'>('device');
  const [name, setName] = useState('');
  const [address, setAddress] = useState('');
  const [profileId, setProfileId] = useState('');
  const [credentialId, setCredentialId] = useState('');
  const [parentId, setParentId] = useState('');
  const [vendor, setVendor] = useState('');
  const [model, setModel] = useState('');
  // URL-monitor fields (used when monType === 'url').
  const [url, setUrl] = useState('');
  const [urlMethod, setUrlMethod] = useState<'GET' | 'HEAD' | 'POST'>('GET');
  const [verifyTls, setVerifyTls] = useState(true);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [credentials, setCredentials] = useState<CredentialSummary[]>([]);
  const [addError, setAddError] = useState<string | null>(null);

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
  const suppression = useMemo(
    () => buildSuppressionIndex(windows, mutes, groups, nodes),
    [windows, mutes, groups, nodes],
  );

  const reload = useCallback(async () => {
    setError(null);
    try {
      const [g, allNodes] = await Promise.all([
        api.listNodeGroups().catch(() => [] as NodeGroup[]),
        loadAllNodes(),
      ]);
      setGroups(g);
      setNodes(allNodes.nodes);
      setTruncated(allNodes.truncated);
    } catch (e: unknown) {
      setError(errMsg(e, 'failed to load nodes'));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

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
        name: `Maintenance — ${target.name}`,
        scope_level: target.kind === 'group' ? 'group_id' : 'node',
        scope_id: target.id,
        starts_at: now.toISOString(),
        ends_at: new Date(now.getTime() + durationMs).toISOString(),
      })
      .then(reloadSuppression)
      .catch((e: unknown) => scopeError(e, 'failed to set maintenance'));
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
      .catch((e: unknown) => scopeError(e, 'failed to mute'));
  };

  // Once loaded, validate the URL selection: keep it if the entity still exists; otherwise fall
  // back to the first problem node (warning/critical/unreachable), else clear it. The fallback is
  // written back to the URL (replace) so a reload lands on the same pane. Runs only when the
  // current selection is missing/stale, so it can't fight a user's choice.
  useEffect(() => {
    if (loading) return;
    const raw = searchParams.get('sel');
    const cur = parseSelection(raw);
    const exists =
      !!cur &&
      (cur.kind === 'node'
        ? nodes.some((n) => n.id === cur.id)
        : groups.some((g) => g.id === cur.id));
    if (exists) return;
    const problem = nodes.find((n) => PROBLEM_STATES.has(n.state));
    const next = problem ? selectionToParam({ kind: 'node', id: problem.id }) : null;
    if (next === (raw ?? null)) return; // already empty / nothing better to select → no write
    const params = new URLSearchParams(searchParams);
    if (next) params.set('sel', next);
    else params.delete('sel');
    params.delete('tab');
    setSearchParams(params, { replace: true });
  }, [loading, nodes, groups, searchParams, setSearchParams]);

  // Load the binding options (profiles + SNMP credentials) when the add-node modal opens.
  useEffect(() => {
    if (!adding) return;
    api.listProfiles().then(setProfiles).catch(() => setProfiles([]));
    api
      .listCredentials()
      .then((c) => setCredentials(c.filter((cr) => cr.kind === 'snmp_v2c')))
      .catch(() => setCredentials([]));
  }, [adding]);

  const resetAddForm = () => {
    setAdding(false);
    setAddGroupId(null);
    setMonType('device');
    setName('');
    setAddress('');
    setProfileId('');
    setCredentialId('');
    setParentId('');
    setVendor('');
    setModel('');
    setUrl('');
    setUrlMethod('GET');
    setVerifyTls(true);
  };

  const submitAdd = () => {
    setAddError(null);
    const created =
      monType === 'url'
        ? api.createUrlMonitor({
            name,
            url,
            method: urlMethod,
            verify_tls: verifyTls,
            parent_id: parentId || undefined,
          })
        : api.createNode({
            name,
            address,
            profile_id: profileId || undefined,
            credential_id: credentialId || undefined,
            parent_id: parentId || undefined,
            vendor: vendor.trim() || undefined,
            model: model.trim() || undefined,
          });
    // createNode/createUrlMonitor take no group_id, so a node lands Ungrouped; if the add was
    // launched from a folder's right-click, place it there with the canonical setNodeGroup op
    // (same as drag-drop). A placement failure is soft — the node still exists, just at top level.
    const groupId = addGroupId;
    created
      .then(({ id }) => (groupId ? api.setNodeGroup(id, groupId) : undefined))
      .then(() => {
        resetAddForm();
        void reload();
      })
      .catch((e: unknown) =>
        setAddError(errMsg(e, monType === 'url' ? 'failed to add URL monitor' : 'failed to add node')),
      );
  };

  // Direct moves (drag-drop): assign immediately and refresh.
  const moveNode = (nodeId: string, groupId: string | null) =>
    api.setNodeGroup(nodeId, groupId).then(reload).catch((e: unknown) =>
      setError(errMsg(e, 'failed to move node')),
    );

  // Nest a group under another (or null = top level), appending it to the end of the destination —
  // the placement endpoint cycle-guards the move and assigns an append order in one call.
  const moveGroup = (groupId: string, parentGroupId: string | null) =>
    api
      .placeNodeGroup(groupId, { parent_id: parentGroupId })
      .then(reload)
      .catch((e: unknown) => setError(errMsg(e, 'failed to move group')));

  // Drag-reorder (before/after a sibling): place the item relative to a neighbour and refresh.
  const reorderNode = (
    nodeId: string,
    dest: { groupId: string | null; before?: string; after?: string },
  ) =>
    api
      .placeNode(nodeId, { group_id: dest.groupId, before: dest.before, after: dest.after })
      .then(reload)
      .catch((e: unknown) => setError(errMsg(e, 'failed to reorder node')));

  const reorderGroup = (
    groupId: string,
    dest: { parentId: string | null; before?: string; after?: string },
  ) =>
    api
      .placeNodeGroup(groupId, { parent_id: dest.parentId, before: dest.before, after: dest.after })
      .then(reload)
      .catch((e: unknown) => setError(errMsg(e, 'failed to reorder group')));

  const nodeCount = nodes.length;
  const attention = tallyStates(nodes).needAttention;
  const selectedNode =
    selected?.kind === 'node' ? nodes.find((n) => n.id === selected.id) ?? null : null;
  const selectedGroup =
    selected?.kind === 'group' ? groups.find((g) => g.id === selected.id) ?? null : null;

  return (
    <div className="page-fill">
      <PageHeader
        title="All nodes"
        trail={[{ label: 'Nodes' }, { label: 'All nodes' }]}
        note={
          <>
            {nodeCount}
            {truncated ? '+' : ''} nodes · {groups.length} groups
            {attention > 0 && <> · <span className="nodes-attention">{attention} need attention</span></>}
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
              Add node
            </Button>
          )
        }
      />

      {error && <p className="form-error">{error}</p>}
      {truncated && (
        <p className="muted nodes-truncated">
          Showing the first {NODE_CAP} nodes. Use search/filter for larger inventories (virtualized
          tree loading is planned).
        </p>
      )}

      <div className="nodes-split">
        <div className="nodes-pane">
          <div className="nodes-pane-head">
            <span className="nodes-pane-title">Inventory</span>
            <div className="nodes-pane-tools">
              {authed && (
                <Button
                  variant="outline"
                  className="nodes-pane-add"
                  title="Add group"
                  onClick={() => setGroupModal({ mode: 'add', parentId: null })}
                >
                  ＋
                </Button>
              )}
              <TextInput
                className="nodes-pane-search"
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                placeholder="Search…"
              />
            </div>
          </div>
          <NodeTree
            groups={groups}
            nodes={nodes}
            canEdit={authed}
            loading={loading}
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
          />
        </div>

        <div className="nodes-pane nodes-detail-pane">
          {selectedNode ? (
            <NodeDetail
              key={selectedNode.id}
              nodeId={selectedNode.id}
              variant="inline"
              canEdit={authed}
              tab={tab}
              onTabChange={setTab}
              groups={groups}
              nodes={nodes}
              onMove={() => setMovingNode(selectedNode)}
              onOpenDetail={() => navigate(`/nodes/${selectedNode.id}`)}
            />
          ) : selectedGroup ? (
            <GroupDetail
              group={selectedGroup}
              groups={groups}
              nodes={nodes}
              canEdit={authed}
              onEditGroup={(g) => setGroupModal({ mode: 'edit', group: g, parentId: g.parent_id })}
              onAddNode={() => setAdding(true)}
            />
          ) : (
            <div className="nd-empty">
              {loading ? 'Loading inventory…' : 'Select a node or group to see its detail.'}
            </div>
          )}
        </div>
      </div>

      {adding && (
        <Modal
          title={monType === 'url' ? 'Add URL monitor' : 'Add node'}
          onClose={resetAddForm}
          footer={
            <>
              <Button onClick={resetAddForm}>Cancel</Button>
              <Button
                variant="primary"
                onClick={submitAdd}
                disabled={!name || (monType === 'url' ? !url : !address)}
              >
                {monType === 'url' ? 'Add URL monitor' : 'Add node'}
              </Button>
            </>
          }
        >
          <div className="form-stack">
            <p className="form-note">
              Adding to{' '}
              <strong>{groups.find((g) => g.id === addGroupId)?.name ?? 'top level'}</strong>.
            </p>
            <label className="form-label">
              Monitoring type
              <Select
                value={monType}
                onChange={(e) => setMonType(e.target.value as 'device' | 'url')}
              >
                <option value="device">Device (ICMP / SNMP)</option>
                <option value="url">URL monitor (HTTP / HTTPS)</option>
              </Select>
            </label>
            <label className="form-label">
              <span>
                Name <RequiredMark />
              </span>
              <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
            </label>
            {monType === 'url' ? (
              <>
                <label className="form-label">
                  <span>
                    URL <RequiredMark />
                  </span>
                  <TextInput
                    className="mono"
                    value={url}
                    onChange={(e) => setUrl(e.target.value)}
                    placeholder="https://api.example.com/health"
                  />
                </label>
                <div className="form-row">
                  <label className="form-label">
                    Method
                    <Select
                      value={urlMethod}
                      onChange={(e) => setUrlMethod(e.target.value as 'GET' | 'HEAD' | 'POST')}
                    >
                      <option value="GET">GET</option>
                      <option value="HEAD">HEAD</option>
                      <option value="POST">POST</option>
                    </Select>
                  </label>
                  <label className="form-label form-check">
                    <input
                      type="checkbox"
                      checked={verifyTls}
                      onChange={(e) => setVerifyTls(e.target.checked)}
                    />
                    <span>Verify TLS certificate</span>
                  </label>
                </div>
              </>
            ) : (
              <>
                <label className="form-label">
                  <span>
                    IP address <RequiredMark />
                  </span>
                  <TextInput
                    className="mono"
                    value={address}
                    onChange={(e) => setAddress(e.target.value)}
                    placeholder="10.0.0.1 or 2001:db8::1"
                  />
                </label>
                <label className="form-label">
                  Device profile (optional)
                  <Select value={profileId} onChange={(e) => setProfileId(e.target.value)}>
                    <option value="">— none —</option>
                    {profiles.map((p) => (
                      <option key={p.id} value={p.id}>
                        {p.name}
                      </option>
                    ))}
                  </Select>
                </label>
                <label className="form-label">
                  SNMP credential (optional — enables SNMP polling)
                  <Select value={credentialId} onChange={(e) => setCredentialId(e.target.value)}>
                    <option value="">— none —</option>
                    {credentials.map((c) => (
                      <option key={c.id} value={c.id}>
                        {c.name}
                      </option>
                    ))}
                  </Select>
                </label>
              </>
            )}
            <label className="form-label">
              Parent node (optional — for dependency suppression)
              <Select value={parentId} onChange={(e) => setParentId(e.target.value)}>
                <option value="">— none —</option>
                {nodes.map((n) => (
                  <option key={n.id} value={n.id}>
                    {n.name}
                  </option>
                ))}
              </Select>
            </label>
            {monType === 'device' && (
              <div className="form-row">
                <label className="form-label">
                  Maker (optional)
                  <TextInput
                    value={vendor}
                    onChange={(e) => setVendor(e.target.value)}
                    placeholder="e.g. Cisco"
                  />
                </label>
                <label className="form-label">
                  Model (optional)
                  <TextInput
                    className="mono"
                    value={model}
                    onChange={(e) => setModel(e.target.value)}
                    placeholder="e.g. C2960"
                  />
                </label>
              </div>
            )}
            {addError && <p className="form-error">{addError}</p>}
          </div>
        </Modal>
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
        <Modal
          title="Delete group"
          onClose={() => setDeletingGroup(null)}
          footer={
            <>
              <Button variant="outline" onClick={() => setDeletingGroup(null)}>
                Cancel
              </Button>
              <Button
                variant="danger"
                onClick={() =>
                  api
                    .deleteNodeGroup(deletingGroup.id)
                    .then(() => {
                      setDeletingGroup(null);
                      void reload();
                    })
                    .catch((e: unknown) => setError(errMsg(e, 'failed to delete group')))
                }
              >
                Delete
              </Button>
            </>
          }
        >
          <p>
            Delete group <strong>{deletingGroup.name}</strong>?{' '}
            {groupDeletionImpact(groups, nodes, deletingGroup)} —{' '}
            <strong>no nodes are deleted</strong>.
          </p>
        </Modal>
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
          nodes={nodes}
          groups={groups}
          initialScope={maintenanceTarget}
          onClose={() => setMaintenanceTarget(null)}
          onSaved={reloadSuppression}
        />
      )}
      {muteTarget && (
        <AddMuteModal
          nodes={nodes}
          groups={groups}
          initialScope={muteTarget}
          onClose={() => setMuteTarget(null)}
          onSaved={reloadSuppression}
        />
      )}
    </div>
  );
}

/** Load all node pages up to NODE_CAP, accumulating into one list for the tree. */
async function loadAllNodes(): Promise<{ nodes: NodeSummary[]; truncated: boolean }> {
  const out: NodeSummary[] = [];
  let cursor: string | undefined;
  for (let i = 0; i < NODE_CAP / PAGE; i++) {
    const page = await api.listNodesPage({ cursor, limit: PAGE });
    out.push(...page.nodes);
    if (!page.next_cursor) return { nodes: out, truncated: false };
    cursor = page.next_cursor;
  }
  return { nodes: out, truncated: true };
}

/** One-line impact summary for deleting a group: how many direct subgroups and member nodes
 *  will be re-parented (nothing is deleted). Pluralised for readability. */
function groupDeletionImpact(groups: NodeGroup[], nodes: NodeSummary[], g: NodeGroup): string {
  const subs = groups.filter((x) => x.parent_id === g.id).length;
  const members = nodes.filter((n) => n.group_id === g.id).length;
  const count = (n: number, word: string) => `${n} ${word}${n === 1 ? '' : 's'}`;
  return `${count(subs, 'subgroup')} and ${count(members, 'member node')} move up to the parent`;
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
  const editing = state.mode === 'edit';
  const [name, setName] = useState(state.group?.name ?? '');
  const [type, setType] = useState<GroupType>(state.group?.group_type ?? 'generic');
  const [parent, setParent] = useState<string>(
    (editing ? state.group?.parent_id : state.parentId) ?? '',
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // For an edit, a group cannot be parented under itself or any of its descendants.
  const parentChoices = groupOptions(groups).filter(
    (o) => !(editing && state.group && isSelfOrDescendant(groups, state.group.id, o.id)),
  );

  const save = () => {
    setBusy(true);
    setError(null);
    const body = { name: name.trim(), group_type: type, parent_id: parent || null };
    const call = editing
      ? api.updateNodeGroup(state.group!.id, body)
      : api.createNodeGroup(body).then(() => undefined);
    call
      .then(onSaved)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to save group'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={editing ? 'Edit group' : 'Add group'}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={save} disabled={!name.trim() || busy}>
            Save
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <label className="form-label">
          <span>
            Name <RequiredMark />
          </span>
          <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
        </label>
        <label className="form-label">
          Type
          <Select value={type} onChange={(e) => setType(e.target.value as GroupType)}>
            {GROUP_TYPES.map((t) => (
              <option key={t.value} value={t.value}>
                {t.label}
              </option>
            ))}
          </Select>
        </label>
        <label className="form-label">
          Parent group
          <Select value={parent} onChange={(e) => setParent(e.target.value)}>
            <option value="">— top level —</option>
            {parentChoices.map((o) => (
              <option key={o.id} value={o.id}>
                {o.label}
              </option>
            ))}
          </Select>
        </label>
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
