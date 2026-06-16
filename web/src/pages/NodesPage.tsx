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

import { useCallback, useEffect, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type {
  CredentialSummary,
  GroupType,
  NodeGroup,
  NodeSummary,
  ProfileSummary,
} from '../types/api';
import { groupOptions, isSelfOrDescendant, tallyStates } from '../lib/nodeTree';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, RequiredMark } from '../components/ui/Field';
import { NodeTree, type TreeSelection } from '../components/NodeTree/NodeTree';
import { NodeDetail } from '../components/NodeDetail/NodeDetail';
import { GroupDetail } from '../components/NodeDetail/GroupDetail';
import { MoveNodeModal } from '../components/MoveNodeModal/MoveNodeModal';
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

  // Split selection (drives the right pane) + the left-pane search filter.
  const [selected, setSelected] = useState<TreeSelection>(null);
  const [filter, setFilter] = useState('');
  // Inline detail's active tab (local; resets to Overview when the selection changes).
  const [tab, setTab] = useState('overview');

  // Add-node modal.
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState('');
  const [address, setAddress] = useState('');
  const [profileId, setProfileId] = useState('');
  const [credentialId, setCredentialId] = useState('');
  const [parentId, setParentId] = useState('');
  const [vendor, setVendor] = useState('');
  const [model, setModel] = useState('');
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [credentials, setCredentials] = useState<CredentialSummary[]>([]);
  const [addError, setAddError] = useState<string | null>(null);

  // Group + move modals.
  const [groupModal, setGroupModal] = useState<GroupModalState | null>(null);
  const [deletingGroup, setDeletingGroup] = useState<NodeGroup | null>(null);
  const [movingNode, setMovingNode] = useState<NodeSummary | null>(null);

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

  // Default + validate the selection: keep the current row if it still exists, otherwise fall back
  // to the first problem node (warning/critical/unreachable), else nothing (empty-state prompt).
  useEffect(() => {
    if (loading) return;
    setSelected((cur) => {
      if (cur) {
        const exists =
          cur.kind === 'node'
            ? nodes.some((n) => n.id === cur.id)
            : groups.some((g) => g.id === cur.id);
        if (exists) return cur;
      }
      const problem = nodes.find((n) => PROBLEM_STATES.has(n.state));
      return problem ? { kind: 'node', id: problem.id } : null;
    });
  }, [loading, nodes, groups]);

  // Reset the inline tab whenever the selected entity changes.
  useEffect(() => {
    setTab('overview');
  }, [selected?.kind, selected?.id]);

  // Load the binding options (profiles + SNMP credentials) when the add-node modal opens.
  useEffect(() => {
    if (!adding) return;
    api.listProfiles().then(setProfiles).catch(() => setProfiles([]));
    api
      .listCredentials()
      .then((c) => setCredentials(c.filter((cr) => cr.kind === 'snmp_v2c')))
      .catch(() => setCredentials([]));
  }, [adding]);

  const submitAdd = () => {
    setAddError(null);
    api
      .createNode({
        name,
        address,
        profile_id: profileId || undefined,
        credential_id: credentialId || undefined,
        parent_id: parentId || undefined,
        vendor: vendor.trim() || undefined,
        model: model.trim() || undefined,
      })
      .then(() => {
        setAdding(false);
        setName('');
        setAddress('');
        setProfileId('');
        setCredentialId('');
        setParentId('');
        setVendor('');
        setModel('');
        void reload();
      })
      .catch((e: unknown) => setAddError(errMsg(e, 'failed to add node')));
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
            <Button variant="primary" onClick={() => setAdding(true)}>
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
            onSelectNode={(n) => setSelected({ kind: 'node', id: n.id })}
            onSelectGroup={(g) => setSelected({ kind: 'group', id: g.id })}
            onOpenNode={(n) => navigate(`/nodes/${n.id}`)}
            onAddGroup={(pid) => setGroupModal({ mode: 'add', parentId: pid })}
            onEditGroup={(g) => setGroupModal({ mode: 'edit', group: g, parentId: g.parent_id })}
            onDeleteGroup={(g) => setDeletingGroup(g)}
            onRequestMoveNode={(n) => setMovingNode(n)}
            onMoveNode={moveNode}
            onMoveGroup={moveGroup}
            onReorderNode={reorderNode}
            onReorderGroup={reorderGroup}
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
          title="Add node"
          onClose={() => setAdding(false)}
          footer={
            <>
              <Button onClick={() => setAdding(false)}>Cancel</Button>
              <Button variant="primary" onClick={submitAdd} disabled={!name || !address}>
                Add node
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
