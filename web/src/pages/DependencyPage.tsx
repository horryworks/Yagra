// SPDX-License-Identifier: AGPL-3.0-only
// Topology ▸ Dependencies. What each node's upstream is, where the hand-authored graph and the
// discovered one differ, and — when suppressed — the root cause it's rolled up under.
//
// This used to be purely an *editing* surface for `nodes.parent_id`. It is now primarily an
// **explanation** surface: the upstream is discovered, and the question an operator has is not "what
// shall I type in" but "do I trust what was found, and what changes if I switch to it". Editing
// stays for as long as the hand-authored graph is the one driving suppression.
//
// Reads: GET /api/v1/topology (the same payload the Network map renders) plus GET
// /api/v1/topology/shadow for the comparison. Kept live via the node-state SSE stream
// (`useNodeStates`, S14). All judgement lives in `topologyDiff.ts` — Vitest never runs a `.tsx`.

import { useCallback, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { api, errMsg } from '../services/api';
import { usePolled } from '../dashboard/usePolled';
import { useNodeStates, LIVE_RECONCILE_MS } from '../dashboard/useNodeStates';
import { useAuthStore } from '../store';
import { formatExactTime, stateColorVar, stateLabel } from '../lib/format';
import type { NodeState, TopologyMode, TopologyNode } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Select } from '../components/ui/Field';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { EntityName } from '../components/ui/EntityName';
import { SetParentModal } from '../components/SetParentModal/SetParentModal';
import { classifyNodes, canEnableDerived, type DiffRow, type DiffVerdict } from './topologyDiff';
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

/** Verdict pill. Colour is a hint; the label carries the meaning (no colour-alone status). */
function VerdictTag({ verdict }: { verdict: DiffVerdict }) {
  const { t } = useTranslation('topology');
  return (
    <span className={`dep-verdict dep-verdict-${verdict}`} title={t(`dependency.verdictHelp.${verdict}`)}>
      {t(`dependency.verdict.${verdict}`)}
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
  const [modeBusy, setModeBusy] = useState(false);
  const [modeError, setModeError] = useState<string | null>(null);
  // Bumped after a save to re-arm usePolled for an immediate refresh (it also reconciles slowly).
  const [refreshNonce, setRefreshNonce] = useState(0);

  const { data, loading, error } = usePolled(
    () => api.getTopology(),
    [refreshNonce],
    LIVE_RECONCILE_MS,
  );
  // The comparison is a separate, slower read: it re-derives the graph server-side, and it is not
  // something the live node-state stream can keep fresh.
  const { data: shadow } = usePolled(() => api.getTopologyShadow(), [refreshNonce], LIVE_RECONCILE_MS);
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

  const mode: TopologyMode = shadow?.mode ?? 'manual';
  const comparing = mode !== 'manual';

  // One classification per node, keyed for the table. `parented` is what separates "both graphs
  // agree this node has an upstream" from "neither graph has an opinion" — see topologyDiff.ts.
  const diffs = useMemo(() => {
    if (!shadow) return new Map<string, DiffRow>();
    const parented = new Set<string>();
    for (const n of nodes) if (n.parent_id) parented.add(n.id);
    for (const e of shadow.only_in_derived ?? []) parented.add(e.child);
    const rows = classifyNodes(shadow, nodes.map((n) => n.id), parented);
    return new Map(rows.map((r) => [r.nodeId, r]));
  }, [shadow, nodes]);

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

  const optedOut = useMemo(() => new Set(shadow?.opted_out ?? []), [shadow]);

  // Memoized so the columns `useMemo` below can depend on it honestly. Behaviour is unchanged
  // either way — `t` is its only unstable capture and was already a dependency there — but a
  // silenced exhaustive-deps warning is one nobody re-reads when a real capture is added later.
  const toggleOptOut = useCallback(
    async (id: string, next: boolean) => {
      setModeError(null);
      try {
        await api.setNodeSuppressionOptOut(id, next);
        setRefreshNonce((v) => v + 1);
      } catch (e) {
        setModeError(errMsg(e, t('dependency.optOutFailed')));
      }
    },
    [t],
  );

  const changeMode = async (next: TopologyMode) => {
    setModeBusy(true);
    setModeError(null);
    try {
      await api.setTopologyMode(next);
      setRefreshNonce((v) => v + 1);
    } catch (e) {
      setModeError(errMsg(e, t('dependency.mode.changeFailed')));
    } finally {
      setModeBusy(false);
    }
  };

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
      ...(comparing
        ? [
            {
              key: 'derived',
              header: t('dependency.cols2.derived'),
              width: '1.4fr',
              render: (r: TopologyNode) => {
                const d = diffs.get(r.id);
                const ids = d?.derivedOnly ?? [];
                if (ids.length === 0) return <span className="dep-none">—</span>;
                return (
                  <span className="dep-derived">
                    {ids.map((id) => (
                      <EntityName key={id} name={nameOf(id) ?? id} id={id} />
                    ))}
                  </span>
                );
              },
            },
            {
              key: 'optout',
              header: t('dependency.cols2.optOut'),
              width: '120px',
              render: (r: TopologyNode) => (
                <label className="dep-optout" title={t('dependency.optOutHelp')}>
                  <input
                    type="checkbox"
                    checked={optedOut.has(r.id)}
                    disabled={!authed}
                    onClick={(e) => e.stopPropagation()}
                    onChange={(e) => {
                      void toggleOptOut(r.id, e.target.checked);
                    }}
                  />
                  <span>{t('dependency.optOut')}</span>
                </label>
              ),
            },
            {
              key: 'verdict',
              header: t('dependency.cols2.verdict'),
              width: '150px',
              render: (r: TopologyNode) => {
                const v = diffs.get(r.id)?.verdict;
                return v ? <VerdictTag verdict={v} /> : <span className="dep-none">—</span>;
              },
            },
          ]
        : []),
      {
        key: 'actions',
        header: t('dependency.cols.actions'),
        width: '110px',
        align: 'right',
        render: (r) =>
          // Hidden once suppression follows the derived graph: editing `parent_id` there changes
          // nothing, and a button that appears to work but does not is worse than no button.
          authed && mode !== 'derived' ? (
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
    [authed, comparing, diffs, mode, nameOf, optedOut, t, toggleOptOut],
  );

  return (
    <div className="page-fill">
      <PageHeader
        title={t('nav:topology.dependency')}
        trail={[{ label: t('nav:sections.topology') }, { label: t('nav:topology.dependency') }]}
        note={t('dependency.note')}
      />

      {shadow && (
        <Card>
          <div className="dep-mode">
            <div className="dep-mode-state">
              <span className="dep-mode-label">{t('dependency.mode.label')}</span>
              <strong className={`dep-mode-value dep-mode-${mode}`}>
                {t(`dependency.mode.${mode}`)}
              </strong>
            </div>
            <p className="muted dep-mode-note">{t(`dependency.mode.${mode}Note`)}</p>

            {comparing && (
              <p className="dep-mode-counts">
                {t('dependency.mode.coverage', {
                  covered: shadow.covered_nodes,
                  total: shadow.total_nodes,
                })}
                {optedOut.size > 0 &&
                  ` · ${t('dependency.mode.optedOut', { count: optedOut.size })}`}
                {shadow.mode_since &&
                  ` · ${t('dependency.mode.since', { at: formatExactTime(shadow.mode_since) })}`}
              </p>
            )}
            {comparing && (
              <p className="dep-mode-counts">
                {t('dependency.mode.edges', {
                  manual: shadow.manual_edges,
                  derived: shadow.derived_edges,
                })}
                {' · '}
                {shadow.would_suppress.length === 0 && shadow.would_unsuppress.length === 0
                  ? t('dependency.mode.noChange')
                  : [
                      shadow.would_suppress.length > 0 &&
                        t('dependency.mode.wouldSuppress', {
                          count: shadow.would_suppress.length,
                        }),
                      shadow.would_unsuppress.length > 0 &&
                        t('dependency.mode.wouldUnsuppress', {
                          count: shadow.would_unsuppress.length,
                        }),
                    ]
                      .filter(Boolean)
                      .join(' ')}
              </p>
            )}

            {/* The blocking condition, stated where the button is. A pool with no placed poller
                contributes no roots, so switching would suppress nothing while looking enabled. */}
            {!canEnableDerived(shadow) && (
              <p className="dep-mode-blocked">
                {t('dependency.mode.blocked', { pools: shadow.unresolved_pools.join(', ') })}
              </p>
            )}
            {mode === 'shadow' && shadow.would_suppress.length > 0 && (
              <p className="dep-mode-blocked">{t('dependency.mode.reviewFirst')}</p>
            )}
            {modeError && <p className="dep-mode-blocked">{modeError}</p>}

            {authed && (
              <div className="dep-mode-actions">
                {mode === 'manual' && (
                  <Button variant="outline" disabled={modeBusy} onClick={() => changeMode('shadow')}>
                    {t('dependency.mode.compare')}
                  </Button>
                )}
                {mode !== 'derived' && (
                  <Button
                    disabled={modeBusy || !canEnableDerived(shadow)}
                    onClick={() => changeMode('derived')}
                  >
                    {t('dependency.mode.enable')}
                  </Button>
                )}
                {mode !== 'manual' && (
                  <Button variant="outline" disabled={modeBusy} onClick={() => changeMode('manual')}>
                    {t('dependency.mode.revert')}
                  </Button>
                )}
              </div>
            )}
          </div>
        </Card>
      )}

      {error ? (
        <Card>
          <p className="muted">{error}</p>
        </Card>
      ) : (
        <>
          {mode === 'derived' && <p className="muted">{t('dependency.editHiddenInDerived')}</p>}
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
