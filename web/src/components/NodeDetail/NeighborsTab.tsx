// SPDX-License-Identifier: AGPL-3.0-only
// Neighbors tab of the unified node detail (ADR-038): what this device currently sees on its ports
// (CDP/LLDP) and when that last changed.
//
// Two sections, because they answer different questions: the table is "what is plugged in where
// right now", the timeline below it is "when did that change". The timeline is normally empty —
// adjacency is recorded append-on-change, so a rack nobody is repatching writes nothing. That is
// the feature, not a gap, and the empty state says so.
//
// All judgement (what counts as a change, why the table is empty, how a peer is labelled) lives in
// the neighbors.ts beside this file: Vitest only runs `src/**/*.test.ts`, so a test written here
// would never execute (testing.md).

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { api, errMsg } from '../../services/api';
import { relativeTime } from '../../lib/format';
import { useRefreshTick } from '../../lib/refreshTick';
import type {
  CurrentNeighbors,
  Neighbor,
  NeighborChange,
  NodeDetail as NodeDetailData,
} from '../../types/api';
import { DataTable, type Column } from '../ui/DataTable';
import { TableToolbar, TableSpacer } from '../ui/TableToolbar';
import { ClearFilters } from '../ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../ui/MobileFilterSheet';
import { useClientFilters } from '../../lib/useClientFilters';
import { neighborFilters } from './tabFilters';
import { nodeTabFilterPrefix } from './tabs';
import {
  diffNeighbors,
  discoveryPath,
  emptyReason,
  neighborAddressState,
  neighborDetails,
  neighborKey,
  neighborLookups,
  peerLabel,
  peerNodePath,
  peerOf,
  peerSecondary,
  platformCell,
  type NeighborDiffRow,
  type NeighborLookups,
} from './neighbors';
import './NeighborsTab.css';

/** How many history rows to load. Adjacency changes are rare, so one page is almost always all of
 *  it; the endpoint is keyset-paged and this tab deliberately does not offer "load more" until a
 *  fleet is seen that needs it. */
const HISTORY_LIMIT = 50;

interface Props {
  node: NodeDetailData;
}

export function NeighborsTab({ node }: Props) {
  const { t } = useTranslation('nodes');
  const tick = useRefreshTick();
  const [current, setCurrent] = useState<CurrentNeighbors | null>(null);
  const [history, setHistory] = useState<NeighborChange[]>([]);
  const [collectionEnabled, setCollectionEnabled] = useState(true);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Client-side: one device has neighbours in the dozens, and the tab already has them all.
  const [sheet, setSheet] = useState(false);

  useEffect(() => {
    let cancelled = false;
    Promise.all([
      api.getNeighbors(node.id),
      api.listNeighborHistory(node.id, { limit: HISTORY_LIMIT }),
    ])
      .then(([cur, page]) => {
        if (cancelled) return;
        setCurrent(cur);
        setHistory(page.changes);
        setError(null);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(errMsg(e, t('neighbors.err.load')));
      })
      .finally(() => {
        if (!cancelled) setLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, [node.id, tick, t]);

  // Whether collection is on is a deployment-wide fact, so it is fetched once rather than on every
  // refresh tick — and a failure here must not blank the tab, only make the empty state vaguer.
  useEffect(() => {
    let cancelled = false;
    api
      .getNeighborSettings()
      .then((s) => {
        if (!cancelled) setCollectionEnabled(s.enabled);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  const neighbors = current?.neighbors.neighbors ?? [];
  const reason = loaded ? emptyReason(collectionEnabled, current) : null;
  // What the server said about each address and MAC (ADR-180), indexed once per response.
  const lookups = useMemo(() => neighborLookups(current), [current]);
  // One row open at a time; clicking it again closes it (ADR-073's "anything selected can be
  // un-selected").
  const [openKey, setOpenKey] = useState<string | null>(null);

  const specs = neighborFilters(t, lookups);
  const columns: Column<Neighbor>[] = [
    {
      key: 'local',
      header: t('neighbors.colLocalPort'),
      width: '1fr',
      render: (n) => (
        <span className="mono nd-nb-line" title={n.local_port}>
          {n.local_port}
        </span>
      ),
    },
    {
      key: 'peer',
      header: t('neighbors.colPeer'),
      width: '1.6fr',
      render: (n) => <PeerCell neighbor={n} lookups={lookups} />,
    },
    {
      key: 'remote_port',
      header: t('neighbors.colRemotePort'),
      width: '1.1fr',
      render: (n) => (
        <span className="nd-nb-stack">
          <span className="mono nd-nb-line" title={n.remote_port || undefined}>
            {n.remote_port || '—'}
          </span>
          {n.remote_port_desc && (
            <span className="nd-muted nd-nb-sub" title={n.remote_port_desc}>
              {n.remote_port_desc}
            </span>
          )}
        </span>
      ),
    },
    {
      key: 'address',
      header: t('neighbors.colAddress'),
      width: '1.1fr',
      render: (n) => <AddressCell neighbor={n} lookups={lookups} />,
    },
    {
      key: 'platform',
      header: t('neighbors.colPlatform'),
      width: '1.4fr',
      render: (n) => <PlatformCell neighbor={n} />,
    },
    {
      key: 'caps',
      header: t('neighbors.colCapabilities'),
      width: '1fr',
      render: (n) => <Capabilities neighbor={n} />,
    },
    {
      key: 'proto',
      header: t('neighbors.colProto'),
      width: '64px',
      render: (n) => (
        <span className="nd-nb-proto" title={t(`neighbors.proto.${n.proto}`)}>
          {t(`neighbors.proto.${n.proto}`)}
        </span>
      ),
    },
  ];
  for (const c of columns) c.filter = specs[c.key];

  // In the URL under `neighbors.` (ADR-153): it survives a reload, and it stays on while the
  // operator walks the tree to the next device — the tab's own "Clear all filters (N)" says why.
  const { filterCols, filters, setFilters, clear, shown: shownNeighbors, counts, anyFiltered } =
    useClientFilters(columns, neighbors, { prefix: nodeTabFilterPrefix('neighbors') });

  return (
    <div className="nd-nb">
      {error && <p className="form-error nd-tabpad">{error}</p>}

      {reason != null ? (
        <p className="nd-muted nd-tabpad">{t(`neighbors.empty.${reason}`)}</p>
      ) : (
        <>
          <div className="nd-nb-summary nd-tabpad">
            <span>{t('neighbors.summary', { count: neighbors.length })}</span>
            {current && (
              <span className="nd-muted">
                {t('neighbors.lastConfirmed', { when: relativeTime(current.last_seen) })}
              </span>
            )}
            {current?.neighbors.truncated && (
              <span className="nd-nb-truncated">{t('neighbors.truncated')}</span>
            )}
            {neighbors.length > 0 && <span className="nd-muted">{t('neighbors.rowHint')}</span>}
          </div>
          <TableToolbar>
            <FilterButton
              columns={filterCols}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters columns={filterCols} filters={filters} onClear={clear} />
            <TableSpacer />
          </TableToolbar>
          <div className="nd-nb-table">
            <DataTable
              tableId="node.neighbors"
              rows={shownNeighbors}
              columns={columns}
              filters={filters}
              onFiltersChange={setFilters}
              filterCounts={counts}
              rowKey={neighborKey}
              loading={!loaded}
              empty={anyFiltered ? t('common:filter.noMatch') : t('neighbors.empty.none')}
              onRowClick={(n) => {
                const key = neighborKey(n);
                setOpenKey((cur) => (cur === key ? null : key));
              }}
              expanded={(n) =>
                neighborKey(n) === openKey ? <Details neighbor={n} lookups={lookups} /> : null
              }
              expandedKey={openKey}
              renderCard={(n) => <NeighborCard neighbor={n} lookups={lookups} />}
            />
          </div>
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={filters}
              onChange={setFilters}
              counts={counts}
              labels={{
                local: t('neighbors.colLocalPort'),
                peer: t('neighbors.colPeer'),
                remote_port: t('neighbors.colRemotePort'),
                address: t('neighbors.colAddress'),
                platform: t('neighbors.colPlatform'),
                proto: t('neighbors.colProto'),
              }}
              onClose={() => setSheet(false)}
            />
          )}
        </>
      )}

      <History changes={history} loaded={loaded} />
    </div>
  );
}

/** Who the neighbour is: its name — a link when exactly one visible node owns its address — then the
 *  chassis id and the chassis maker underneath (ADR-180). */
function PeerCell({ neighbor: n, lookups }: { neighbor: Neighbor; lookups: NeighborLookups }) {
  const peer = peerOf(n, lookups);
  const path = peerNodePath(peer);
  const label = peerLabel(n);
  const secondary = peerSecondary(n, lookups);
  return (
    <span className="nd-nb-stack">
      {path ? (
        <Link
          to={path}
          className="nd-nb-line nd-nb-link"
          title={peer?.node_name ?? label}
          // A link inside a clickable row must not also open or close the row.
          onClick={(e) => e.stopPropagation()}
        >
          {label}
        </Link>
      ) : (
        <span className="nd-nb-line" title={label}>
          {label}
        </span>
      )}
      {secondary && (
        <span className="nd-muted nd-nb-sub" title={secondary}>
          {secondary}
        </span>
      )}
    </span>
  );
}

/** The management address and what it is to this deployment. */
function AddressCell({ neighbor: n, lookups }: { neighbor: Neighbor; lookups: NeighborLookups }) {
  const { t } = useTranslation('nodes');
  const state = neighborAddressState(n, lookups);
  const discovery = discoveryPath(peerOf(n, lookups));
  return (
    <span className="nd-nb-stack">
      <span className="mono nd-nb-line" title={n.remote_mgmt_addr ?? undefined}>
        {n.remote_mgmt_addr ?? '—'}
      </span>
      {state && state !== 'none' && (
        <span className="nd-nb-sub">
          <span
            className={`nd-nb-state ${state}`}
            title={t(`neighbors.peer.explain.${state}`)}
          >
            {t(`neighbors.peer.state.${state}`)}
          </span>
          {discovery && (
            <>
              {' '}
              <Link to={discovery} className="nd-nb-link" onClick={(e) => e.stopPropagation()}>
                {t('neighbors.peer.inDiscovery')}
              </Link>
            </>
          )}
        </span>
      )}
    </span>
  );
}

/** The model / OS: CDP's platform over its version banner, or LLDP's system description. */
function PlatformCell({ neighbor: n }: { neighbor: Neighbor }) {
  const { primary, secondary } = platformCell(n);
  if (!primary) return <span className="nd-muted">—</span>;
  return (
    <span className="nd-nb-stack">
      <span className="nd-nb-line" title={primary}>
        {primary}
      </span>
      {secondary && (
        <span className="nd-muted nd-nb-sub" title={secondary}>
          {secondary}
        </span>
      )}
    </span>
  );
}

/** Everything the neighbour sent, in full and wrapped — what the ellipsized cells above leave out. */
function Details({ neighbor: n, lookups }: { neighbor: Neighbor; lookups: NeighborLookups }) {
  const { t } = useTranslation('nodes');
  return (
    <div className="nd-nb-details">
      <dl className="nd-nb-dl">
        {neighborDetails(n, lookups).map((d) => (
          <div key={d.labelKey} className="nd-nb-dl-row">
            <dt>{t(`neighbors.detail.${d.labelKey}`)}</dt>
            <dd className={d.mono ? 'mono' : undefined}>{d.value}</dd>
          </div>
        ))}
      </dl>
      <p className="nd-muted nd-nb-note">{t('neighbors.detail.note')}</p>
    </div>
  );
}

/** The phone layout: the four facts in reading order, and the full record behind a disclosure —
 *  a phone has no hover to read an ellipsized cell with. */
function NeighborCard({ neighbor: n, lookups }: { neighbor: Neighbor; lookups: NeighborLookups }) {
  const { t } = useTranslation('nodes');
  const [open, setOpen] = useState(false);
  const { primary } = platformCell(n);
  return (
    <div className="nd-nb-card">
      <PeerCell neighbor={n} lookups={lookups} />
      <span className="mono nd-nb-card-ports">
        {n.local_port} → {n.remote_port || '—'}
      </span>
      <AddressCell neighbor={n} lookups={lookups} />
      {primary && <span className="nd-nb-card-platform">{primary}</span>}
      <span className="nd-nb-card-chips">
        <Capabilities neighbor={n} />
        <span className="nd-nb-proto">{t(`neighbors.proto.${n.proto}`)}</span>
      </span>
      <button
        type="button"
        className="nd-nb-card-toggle"
        aria-expanded={open}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
      >
        {open ? t('neighbors.detail.hide') : t('neighbors.detail.show')}
      </button>
      {open && <Details neighbor={n} lookups={lookups} />}
    </div>
  );
}

/** The peer's advertised roles, as chips. Rendered from the tokens the backend normalized both
 *  protocols onto, so there is no per-protocol legend to keep in sync. Also drawn by the Interfaces
 *  list's neighbour popover (ADR-145), so the two surfaces cannot name a role differently. */
export function Capabilities({ neighbor }: { neighbor: Neighbor }) {
  const { t } = useTranslation('nodes');
  const caps = neighbor.capabilities ?? [];
  if (caps.length === 0) return <span className="nd-muted">—</span>;
  return (
    <span className="nd-nb-caps">
      {caps.map((c) => (
        <span key={c} className="nd-nb-cap">
          {t(`neighbors.capability.${c}`, { defaultValue: c })}
        </span>
      ))}
    </span>
  );
}

/** The append-on-change timeline. Each entry says what moved relative to the observation before it;
 *  the oldest recorded entry has no predecessor, so every link in it reads as new. */
function History({ changes, loaded }: { changes: NeighborChange[]; loaded: boolean }) {
  const { t } = useTranslation('nodes');
  if (!loaded) return null;
  return (
    <div className="nd-nb-history nd-tabpad">
      <h3 className="nd-nb-h3">{t('neighbors.historyTitle')}</h3>
      {changes.length === 0 ? (
        <p className="nd-muted">{t('neighbors.historyEmpty')}</p>
      ) : (
        <ol className="nd-nb-timeline">
          {changes.map((c, i) => {
            // The page is newest-first, so the observation *before* this one is the next element.
            // The last element on the page has no predecessor here — for the genesis row that is
            // genuinely true, and otherwise it just means the previous change is on the next page,
            // where showing every link as new is the honest reading of what we loaded.
            const previous = changes[i + 1]?.neighbors ?? null;
            const rows = diffNeighbors(previous, c.neighbors);
            return (
              <li key={c.id} className="nd-nb-entry">
                <div className="nd-nb-when" title={c.at}>
                  {relativeTime(c.at)}
                </div>
                <ul className="nd-nb-diff">
                  {rows.map((r) => (
                    <DiffLine key={`${r.kind}-${neighborKey(r.neighbor)}`} row={r} />
                  ))}
                  {rows.length === 0 && (
                    <li className="nd-muted">{t('neighbors.diff.unchangedDetail')}</li>
                  )}
                </ul>
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}

function DiffLine({ row }: { row: NeighborDiffRow }) {
  const { t } = useTranslation('nodes');
  return (
    <li className={`nd-nb-diffline ${row.kind}`}>
      <span className="nd-nb-diffkind">{t(`neighbors.diff.${row.kind}`)}</span>
      <span className="mono">{row.neighbor.local_port}</span>
      <span className="nd-muted">→</span>
      <span>{peerLabel(row.neighbor)}</span>
      {row.neighbor.remote_port && <span className="mono">{row.neighbor.remote_port}</span>}
    </li>
  );
}
