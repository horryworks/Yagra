// Nodes / All nodes. Per the agreed scope this is a real-data flat table (not the folder
// tree — the backend has no folder concept yet). Keyset pagination feeds the virtualized
// DataTable: pages load on demand as the operator scrolls, so it stays usable at tens of
// thousands of rows. Row click drills into node detail. Add-node is a focused-edit modal
// (ManageConfig); 503 in skeleton mode is surfaced, not swallowed.

import { useCallback, useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CredentialSummary, NodeSummary, ProfileSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { StatusDot } from '../components/ui/StatusDot';
import { DataTable, type Column } from '../components/ui/DataTable';

const PAGE = 100;

const COLUMNS: Column<NodeSummary>[] = [
  { key: 'state', header: 'State', width: '120px', render: (n) => <StatusDot state={n.state} /> },
  { key: 'name', header: 'Name', width: '1.4fr', render: (n) => n.name },
  {
    key: 'address',
    header: 'Address',
    width: '1fr',
    render: (n) => <span className="mono">{n.address}</span>,
  },
];

export function NodesPage() {
  const navigate = useNavigate();
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<NodeSummary[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const loading = useRef(false);

  const [adding, setAdding] = useState(false);
  const [name, setName] = useState('');
  const [address, setAddress] = useState('');
  const [profileId, setProfileId] = useState('');
  const [credentialId, setCredentialId] = useState('');
  const [parentId, setParentId] = useState('');
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [credentials, setCredentials] = useState<CredentialSummary[]>([]);
  const [addError, setAddError] = useState<string | null>(null);

  const loadMore = useCallback(() => {
    if (loading.current || done) return;
    loading.current = true;
    api
      .listNodesPage({ cursor: cursor ?? undefined, limit: PAGE })
      .then((page) => {
        setRows((cur) => [...cur, ...page.nodes]);
        setCursor(page.next_cursor);
        if (!page.next_cursor) setDone(true);
      })
      .catch((e: unknown) => setError(e instanceof ApiError ? e.message : 'failed to load nodes'))
      .finally(() => {
        loading.current = false;
      });
  }, [cursor, done]);

  // Reset + first page on mount.
  const reload = useCallback(() => {
    setRows([]);
    setCursor(null);
    setDone(false);
    setError(null);
    loading.current = false;
    api
      .listNodesPage({ limit: PAGE })
      .then((page) => {
        setRows(page.nodes);
        setCursor(page.next_cursor);
        if (!page.next_cursor) setDone(true);
      })
      .catch((e: unknown) => setError(e instanceof ApiError ? e.message : 'failed to load nodes'));
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  // Load the binding options (device profiles + SNMP credentials) when the modal opens, so
  // a node can be put under monitoring with its SNMP community in one step. Best-effort:
  // a viewer (or skeleton mode) without access just sees empty pickers.
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
      })
      .then(() => {
        setAdding(false);
        setName('');
        setAddress('');
        setProfileId('');
        setCredentialId('');
        setParentId('');
        reload();
      })
      .catch((e: unknown) =>
        setAddError(e instanceof ApiError ? e.message : 'failed to add node'),
      );
  };

  return (
    <div className="page-fill">
      <PageHeader
        title="All nodes"
        trail={[{ label: 'Nodes' }, { label: 'All nodes' }]}
        note={`${rows.length}${done ? '' : '+'} nodes`}
        actions={
          authed && (
            <Button variant="primary" onClick={() => setAdding(true)}>
              Add node
            </Button>
          )
        }
      />

      <Card className="page-fill-card">
        {error && <p className="muted">{error}</p>}
        <DataTable
          rows={rows}
          columns={COLUMNS}
          rowKey={(n) => n.id}
          onReachEnd={loadMore}
          onRowClick={(n) => navigate(`/nodes/${n.id}`)}
          empty="No nodes in inventory. Add one to start monitoring."
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
                {rows.map((n) => (
                  <option key={n.id} value={n.id}>
                    {n.name}
                  </option>
                ))}
              </Select>
            </label>
            {addError && <p className="form-error">{addError}</p>}
          </div>
        </Modal>
      )}
    </div>
  );
}
