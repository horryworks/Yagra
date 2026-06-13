// Nodes / All nodes. A hierarchical inventory tree (groups → member nodes), modelled on HoTTY's
// HostTree: add/rename/delete typed groups (each type has its own icon), and move nodes/groups by
// drag-drop, a context menu, or a "Move to…" picker. Row click drills into node detail. Add-node
// and group edits are focused-edit modals (ManageConfig); 503 in skeleton mode is surfaced.
//
// Scale note: the tree needs the full group + node sets, so it loads all node pages up to a cap
// (NODE_CAP) and flags when an inventory is larger than that — virtualized-per-group lazy loading
// is the follow-up for very large fleets. Groups are few and load whole.

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
import { isSelfOrDescendant } from '../lib/nodeTree';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { NodeTree } from '../components/NodeTree/NodeTree';

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
  const [error, setError] = useState<string | null>(null);

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
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

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

  const moveGroup = (groupId: string, parentGroupId: string | null) => {
    const g = groups.find((x) => x.id === groupId);
    if (!g) return;
    api
      .updateNodeGroup(groupId, { name: g.name, group_type: g.group_type, parent_id: parentGroupId })
      .then(reload)
      .catch((e: unknown) => setError(errMsg(e, 'failed to move group')));
  };

  const nodeCount = nodes.length;

  return (
    <div className="page-fill">
      <PageHeader
        title="All nodes"
        trail={[{ label: 'Nodes' }, { label: 'All nodes' }]}
        note={`${nodeCount}${truncated ? '+' : ''} nodes · ${groups.length} groups`}
        actions={
          authed && (
            <Button variant="primary" onClick={() => setAdding(true)}>
              Add node
            </Button>
          )
        }
      />

      <Card className="page-fill-card">
        {error && <p className="form-error">{error}</p>}
        {truncated && (
          <p className="muted">
            Showing the first {NODE_CAP} nodes. Use search/filter for larger inventories
            (virtualized tree loading is planned).
          </p>
        )}
        <NodeTree
          groups={groups}
          nodes={nodes}
          canEdit={authed}
          onOpenNode={(n) => navigate(`/nodes/${n.id}`)}
          onAddGroup={(pid) => setGroupModal({ mode: 'add', parentId: pid })}
          onEditGroup={(g) => setGroupModal({ mode: 'edit', group: g, parentId: g.parent_id })}
          onDeleteGroup={(g) => setDeletingGroup(g)}
          onRequestMoveNode={(n) => setMovingNode(n)}
          onMoveNode={moveNode}
          onMoveGroup={moveGroup}
        />
      </Card>

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
              Name
              <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
            </label>
            <label className="form-label">
              IP address
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
            Delete group <strong>{deletingGroup.name}</strong>? Its subgroups and member nodes move
            up to the parent — <strong>no nodes are deleted</strong>.
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

/** Group label for a select option, indented by depth so the hierarchy reads in a flat list. */
function groupOptions(groups: NodeGroup[]): { id: string; label: string }[] {
  const byParent = new Map<string | null, NodeGroup[]>();
  for (const g of groups) {
    const k = g.parent_id;
    byParent.set(k, [...(byParent.get(k) ?? []), g]);
  }
  const out: { id: string; label: string }[] = [];
  const walk = (parent: string | null, depth: number) => {
    for (const g of (byParent.get(parent) ?? []).sort((a, b) => a.name.localeCompare(b.name))) {
      out.push({ id: g.id, label: `${'  '.repeat(depth)}${g.name}` });
      walk(g.id, depth + 1);
    }
  };
  walk(null, 0);
  return out;
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
          Name
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

/** Move a node into a group (or ungroup it). */
function MoveNodeModal({
  node,
  groups,
  onClose,
  onMoved,
}: {
  node: NodeSummary;
  groups: NodeGroup[];
  onClose: () => void;
  onMoved: () => void;
}) {
  const [target, setTarget] = useState<string>(node.group_id ?? '');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const save = () => {
    setBusy(true);
    setError(null);
    api
      .setNodeGroup(node.id, target || null)
      .then(onMoved)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to move node'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={`Move ${node.name}`}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={save} disabled={busy}>
            Move
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <label className="form-label">
          Group
          <Select value={target} onChange={(e) => setTarget(e.target.value)}>
            <option value="">— Ungrouped —</option>
            {groupOptions(groups).map((o) => (
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
