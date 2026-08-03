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

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
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
import {
  diffNeighbors,
  emptyReason,
  neighborKey,
  peerLabel,
  peerLabelIsChassis,
  type NeighborDiffRow,
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

  const columns: Column<Neighbor>[] = [
    {
      key: 'local',
      header: t('neighbors.colLocalPort'),
      width: '1.1fr',
      render: (n) => <span className="mono">{n.local_port}</span>,
    },
    {
      key: 'peer',
      header: t('neighbors.colPeer'),
      width: '1.4fr',
      render: (n) => (
        <span className="nd-nb-peer">
          <span className="nd-nb-peername">{peerLabel(n)}</span>
          {!peerLabelIsChassis(n) && <span className="mono nd-muted">{n.remote_chassis}</span>}
        </span>
      ),
    },
    {
      key: 'remote_port',
      header: t('neighbors.colRemotePort'),
      width: '1.1fr',
      render: (n) => <span className="mono">{n.remote_port || '—'}</span>,
    },
    {
      key: 'caps',
      header: t('neighbors.colCapabilities'),
      width: '1.2fr',
      render: (n) => <Capabilities neighbor={n} />,
    },
    {
      key: 'proto',
      header: t('neighbors.colProto'),
      width: '80px',
      render: (n) => (
        <span className="nd-nb-proto" title={t(`neighbors.proto.${n.proto}`)}>
          {t(`neighbors.proto.${n.proto}`)}
        </span>
      ),
    },
  ];

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
          </div>
          <div className="nd-nb-table">
            <DataTable
              rows={neighbors}
              columns={columns}
              rowKey={neighborKey}
              loading={!loaded}
              empty={t('neighbors.empty.none')}
            />
          </div>
        </>
      )}

      <History changes={history} loaded={loaded} />
    </div>
  );
}

/** The peer's advertised roles, as chips. Rendered from the tokens the backend normalized both
 *  protocols onto, so there is no per-protocol legend to keep in sync. */
function Capabilities({ neighbor }: { neighbor: Neighbor }) {
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
