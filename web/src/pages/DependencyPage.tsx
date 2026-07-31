// SPDX-License-Identifier: AGPL-3.0-only
// Topology ▸ Dependencies. The management surface for the dependency graph: every node, its
// upstream (parent), live status, and — when suppressed — the root cause it's rolled up under.
// Set/change/clear a node's upstream inline (the shared SetParentModal). Read: GET /api/v1/topology
// (the same payload the Network map renders), fetched on a slow reconcile cadence and kept live via
// the node-state SSE stream (`useNodeStates`, S14); the map stays the visualization, this is where
// you edit the edges. Names are resolved locally from the payload (every node is present), so no
// raw UUIDs are shown.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { api } from '../services/api';
import { usePolled } from '../dashboard/usePolled';
import { useNodeStates, LIVE_RECONCILE_MS } from '../dashboard/useNodeStates';
import { useAuthStore } from '../store';
import { stateColorVar, stateLabel } from '../lib/format';
import type { NodeState, TopologyNode } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Select } from '../components/ui/Field';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { EntityName } from '../components/ui/EntityName';
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
  const { t } = useTranslation('topology');
  const authed = useAuthStore((s) => s.authed);
  const navigate = useNavigate();
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<'all' | 'upstream' | 'suppressed'>('all');
  const [editing, setEditing] = useState<TopologyNode | null>(null);
  // Bumped after a save to re-arm usePolled for an immediate refresh (it also reconciles slowly).
  const [refreshNonce, setRefreshNonce] = useState(0);

  const { data, loading, error } = usePolled(
    () => api.getTopology(),
    [refreshNonce],
    LIVE_RECONCILE_MS,
  );
  const live = useNodeStates();
  // Overlay the live SSE state on top of the fetched graph (S14): a fresh event wins over the
  // slower structural refetch's snapshot.
  const nodes = useMemo(() => {
    const base = data?.nodes ?? [];
    return base.map((n) => {
      const s = live.get(n.id);
      return s && s !== n.state ? { ...n, state: s } : n;
    });
  }, [data, live]);
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
        header: t('dependency.cols.node'),
        width: '1.4fr',
        render: (r) => (
          <span className="dep-name" title={r.id}>
            {r.name}
          </span>
        ),
      },
      {
        key: 'upstream',
        header: t('dependency.cols.upstream'),
        width: '1.4fr',
        render: (r) =>
          r.parent_id ? (
            <EntityName name={nameOf(r.parent_id) ?? r.parent_id} id={r.parent_id} />
          ) : (
            <span className="dep-none">—</span>
          ),
      },
      { key: 'status', header: t('dependency.cols.status'), width: '140px', render: (r) => <StatusTag state={r.state} /> },
      {
        key: 'root',
        header: t('dependency.cols.root'),
        width: '1.4fr',
        render: (r) =>
          r.root_cause ? (
            <EntityName name={nameOf(r.root_cause) ?? r.root_cause} id={r.root_cause} />
          ) : (
            <span className="dep-none">—</span>
          ),
      },
      {
        key: 'actions',
        header: t('dependency.cols.actions'),
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
              {t('common:actions.edit')}
            </Button>
          ) : null,
      },
    ],
    [authed, nameOf, t],
  );

  return (
    <div className="page-fill">
      <PageHeader
        title={t('nav:topology.dependency')}
        trail={[{ label: t('nav:sections.topology') }, { label: t('nav:topology.dependency') }]}
        note={t('dependency.note')}
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
              placeholder={t('dependency.searchPlaceholder')}
              ariaLabel={t('dependency.searchAria')}
            />
            <Select
              value={filter}
              onChange={(e) => setFilter(e.target.value as typeof filter)}
              aria-label={t('dependency.filterAria')}
            >
              <option value="all">{t('dependency.filter.all')}</option>
              <option value="upstream">{t('dependency.filter.upstream')}</option>
              <option value="suppressed">{t('dependency.filter.suppressed')}</option>
            </Select>
            <TableSpacer />
            <ResultCount shown={rows.length} noun={t('common:noun.node', { count: rows.length })} />
          </TableToolbar>

          <DataTable
            rows={rows}
            columns={columns}
            rowKey={(r) => r.id}
            onRowClick={(r) => navigate(`/nodes/${r.id}`)}
            loading={loading}
            empty={
              nodes.length === 0
                ? t('dependency.emptyInventory')
                : t('dependency.emptyFiltered')
            }
          />
        </>
      )}

      {editing && (
        <SetParentModal
          nodeId={editing.id}
          nodeName={editing.name}
          currentParentId={editing.parent_id ?? null}
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
