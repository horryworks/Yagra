// Topology ▸ Dependencies. The management surface for the dependency graph: every node, its
// upstream (parent), live status, and — when suppressed — the root cause it's rolled up under.
// Set/change/clear a node's upstream inline (the shared SetParentModal). Read: GET /api/v1/topology
// (the same payload the Network map renders), polled on the dashboard cadence; the map stays the
// visualization, this is where you edit the edges. Names are resolved locally from the payload
// (every node is present), so no raw UUIDs are shown.

import { useMemo, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { api } from '../services/api';
import { usePolled } from '../dashboard/usePolled';
import { useAuthStore } from '../store';
import { stateColorVar, stateLabel } from '../lib/format';
import type { NodeState, TopologyNode } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Select } from '../components/ui/Field';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { SetParentModal } from '../components/SetParentModal/SetParentModal';
import './DependencyPage.css';

/** Small status pill (dot + label, colored by the state variable — never color alone). */
function StatusTag({ state }: { state: NodeState }) {
  return (
    <span className="dep-status" style={{ color: stateColorVar(state) }}>
      <span className="dep-dot" />
      {stateLabel(state)}
    </span>
  );
}

export function DependencyPage() {
  const authed = useAuthStore((s) => s.authed);
  const navigate = useNavigate();
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<'all' | 'upstream' | 'suppressed'>('all');
  const [editing, setEditing] = useState<TopologyNode | null>(null);
  // Bumped after a save to re-arm usePolled for an immediate refresh (it also polls every 15s).
  const [refreshNonce, setRefreshNonce] = useState(0);

  const { data, loading, error } = usePolled(() => api.getTopology(), [refreshNonce]);
  const nodes = useMemo(() => data?.nodes ?? [], [data]);
  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const n of nodes) m.set(n.id, n.name);
    return (id: string | null): string | null => (id ? m.get(id) ?? id : null);
  }, [nodes]);

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    return nodes.filter((n) => {
      const matchesQuery = q === '' || n.name.toLowerCase().includes(q);
      const matchesFilter =
        filter === 'all' ||
        (filter === 'upstream' ? n.parent_id != null : n.root_cause != null);
      return matchesQuery && matchesFilter;
    });
  }, [nodes, query, filter]);

  const columns: Column<TopologyNode>[] = useMemo(
    () => [
      {
        key: 'node',
        header: 'Node',
        width: '1.4fr',
        render: (r) => (
          <span className="dep-name" title={r.id}>
            {r.name}
          </span>
        ),
      },
      {
        key: 'upstream',
        header: 'Depends on',
        width: '1.4fr',
        render: (r) =>
          r.parent_id ? (
            <span title={r.parent_id}>{nameOf(r.parent_id)}</span>
          ) : (
            <span className="dep-none">—</span>
          ),
      },
      { key: 'status', header: 'Status', width: '140px', render: (r) => <StatusTag state={r.state} /> },
      {
        key: 'root',
        header: 'Root cause',
        width: '1.4fr',
        render: (r) =>
          r.root_cause ? (
            <span title={r.root_cause}>{nameOf(r.root_cause)}</span>
          ) : (
            <span className="dep-none">—</span>
          ),
      },
      {
        key: 'actions',
        header: 'Actions',
        width: '110px',
        align: 'right',
        render: (r) =>
          authed ? (
            <Button
              variant="outline"
              onClick={(e) => {
                e.stopPropagation();
                setEditing(r);
              }}
            >
              Edit
            </Button>
          ) : null,
      },
    ],
    [authed, nameOf],
  );

  return (
    <div className="page-fill">
      <PageHeader
        title="Dependencies"
        trail={[{ label: 'Topology' }, { label: 'Dependencies' }]}
        note="Manage the dependency graph: each node's upstream drives parent-down alert suppression and root-cause roll-up. The Network map visualizes the same edges."
      />

      {error ? (
        <Card>
          <p className="muted">{error}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder="Search nodes by name…"
              ariaLabel="Search nodes"
            />
            <Select
              value={filter}
              onChange={(e) => setFilter(e.target.value as typeof filter)}
              aria-label="Filter dependencies"
            >
              <option value="all">All nodes</option>
              <option value="upstream">With upstream</option>
              <option value="suppressed">Currently suppressed</option>
            </Select>
            <TableSpacer />
            <ResultCount shown={rows.length} noun="nodes" />
          </TableToolbar>

          <DataTable
            rows={rows}
            columns={columns}
            rowKey={(r) => r.id}
            onRowClick={(r) => navigate(`/nodes/${r.id}`)}
            loading={loading}
            empty={
              nodes.length === 0
                ? 'No nodes in the inventory yet.'
                : 'No matching nodes — adjust the search or filter.'
            }
          />
        </>
      )}

      {editing && (
        <SetParentModal
          nodeId={editing.id}
          nodeName={editing.name}
          currentParentId={editing.parent_id}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            setRefreshNonce((v) => v + 1);
          }}
        />
      )}
    </div>
  );
}
