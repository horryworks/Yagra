// SPDX-License-Identifier: AGPL-3.0-only
// DNS-monitor section of the node Overview (ADR-033). Shown only for DNS-monitor nodes (the caller
// guards on `node.dns_check`), mirroring how UrlHealth self-hides.
//
// Two things it shows that no other section does:
//   1. the recursive resolution chain (`horryworks.net → CNAME horry.net → A 10.1.2.3`), rendered
//      as a dig-like strip;
//   2. its history — and because core only appends a row when the chain's canonical content
//      actually changes, that history is a log of real changes, not of polls.
//
// Metric names are hardcoded here rather than resolved from the node's collection set, because a
// DNS monitor has no collection set at all (its profile carries no SNMP templates) — the same
// reason UrlHealth hardcodes `http_up`.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import { formatTimestamp, pointsToSeries } from '../../lib/format';
import { MetricChart } from '../MetricChart/MetricChart';
import { Badge } from '../ui/Badge';
import { Column, DataTable } from '../ui/DataTable';
import { RangeControl, resolveRange } from './RangeControl';
import { useRangeStore } from '../../store';
import { useRefreshTick } from '../../lib/refreshTick';
import { chainSummary, chainToRows, failureLabel } from './dnsChain';
import type { DnsChainChange, DnsChainCurrent, DnsCheckConfig } from '../../types/api';
import './DnsHealth.css';

/** How many history rows to show. Append-on-change keeps this list short in practice. */
const HISTORY_LIMIT = 50;

export function DnsHealth({ nodeId, check }: { nodeId: string; check: DnsCheckConfig }) {
  const { t } = useTranslation('nodes');
  const range = useRangeStore((s) => s.range);
  const setRange = useRangeStore((s) => s.setRange);
  const tick = useRefreshTick();
  const [up, setUp] = useState<number | null>(null);
  const [resolveMs, setResolveMs] = useState<number | null>(null);
  const [current, setCurrent] = useState<DnsChainCurrent | null>(null);
  const [history, setHistory] = useState<DnsChainChange[]>([]);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [win, setWin] = useState<[number, number] | null>(null);

  useEffect(() => {
    let cancelled = false;
    const { from, to } = resolveRange(range);
    void Promise.allSettled([
      api.getNodeMetric(nodeId, 'dns_up'),
      api.getNodeMetric(nodeId, 'dns_resolve_ms'),
      api.getNodeMetricRange(nodeId, 'dns_resolve_ms', { from, to }),
      api.getDnsChain(nodeId),
      api.listDnsChainHistory(nodeId, { limit: HISTORY_LIMIT }),
    ]).then(([u, ms, r, chain, hist]) => {
      if (cancelled) return;
      setUp(u.status === 'fulfilled' ? u.value.value : null);
      setResolveMs(ms.status === 'fulfilled' ? ms.value.value : null);
      setSeries(
        r.status === 'fulfilled' ? pointsToSeries(r.value.points) : { timestamps: [], values: [] },
      );
      setCurrent(chain.status === 'fulfilled' ? chain.value : null);
      setHistory(hist.status === 'fulfilled' ? hist.value.changes : []);
      setWin([from, to]);
    });
    return () => {
      cancelled = true;
    };
  }, [nodeId, range, tick]);

  const resolves = up === 1;
  const rows = current ? chainToRows(current.chain) : [];
  const failure = current ? failureLabel(current.chain.failure ?? null) : null;

  const historyColumns: Column<DnsChainChange>[] = [
    {
      key: 'at',
      header: t('overview.changedAt'),
      width: '180px',
      render: (row) => formatTimestamp(Date.parse(row.at)),
    },
    {
      key: 'outcome',
      header: t('overview.resolution'),
      width: '140px',
      render: (row) => (
        <Badge tone={row.resolved ? 'up' : 'critical'}>
          {row.resolved ? t('overview.resolves') : (row.failure_kind ?? t('overview.doesNotResolve'))}
        </Badge>
      ),
    },
    {
      key: 'chain',
      header: t('overview.resolutionChain'),
      width: '1fr',
      render: (row) => <span className="dns-history-summary">{chainSummary(row.chain)}</span>,
    },
  ];

  return (
    <section>
      <div className="nd-section-head">
        <div className="nd-section-t">{t('overview.dnsMonitor')}</div>
        <RangeControl value={range} onChange={setRange} />
      </div>

      <div className="dns-target">
        <span className="nd-fact-v mono">{check.name}</span>
        <span className="dns-target-meta">
          {check.record_type} · {check.resolver ?? t('overview.systemResolver')}
        </span>
      </div>

      <div className="nd-health-metrics">
        <div className="nd-health-metric">
          <div className="nd-health-metric-head">
            <span className="nd-health-metric-label">{t('overview.resolution')}</span>
            <span
              className="nd-health-metric-value"
              style={{ color: resolves ? 'var(--status-ok)' : 'var(--status-critical)' }}
            >
              {up == null ? '—' : resolves ? t('overview.resolves') : t('overview.doesNotResolve')}
            </span>
          </div>
          {series.timestamps.length > 0 && (
            <MetricChart
              title={t('overview.resolveTime')}
              timestamps={series.timestamps}
              values={series.values}
              yFormat={(v) => `${Math.round(v)} ms`}
              legendFormat={(v) => `${Math.round(v)} ms`}
              xRange={win ?? undefined}
            />
          )}
        </div>
        {resolveMs != null && (
          <div className="nd-health-metric">
            <div className="nd-health-metric-head">
              <span className="nd-health-metric-label">{t('overview.resolveTime')}</span>
              <span className="nd-health-metric-value">{Math.round(resolveMs)} ms</span>
            </div>
          </div>
        )}
      </div>

      {/* The resolution chain, dig-style. */}
      {current == null ? (
        <div className="dns-chain-empty">{t('overview.noChainYet')}</div>
      ) : (
        <>
          <div className="dns-chain">
            {rows.map((row, i) => (
              <div className="dns-chain-step" key={`${row.name}-${i}`}>
                <div className="dns-hop">
                  <span>{row.name}</span>
                </div>
                {row.values.length > 0 && (
                  <>
                    <span className="dns-edge">
                      <span>{row.edge}</span>
                      <span className="dns-edge-arrow" aria-hidden="true">
                        →
                      </span>
                    </span>
                    <div
                      className={`dns-hop${row.terminal ? ' terminal' : ''}`}
                      title={row.ttl == null ? undefined : `TTL ${row.ttl}s`}
                    >
                      {row.values.map((v) => (
                        <span className="dns-hop-value" key={v}>
                          {v}
                        </span>
                      ))}
                      {row.ttl != null && <span className="dns-ttl">TTL {row.ttl}s</span>}
                    </div>
                  </>
                )}
              </div>
            ))}
            {failure && (
              <div className="dns-chain-step">
                {rows.length > 0 && (
                  <span className="dns-edge">
                    <span className="dns-edge-arrow" aria-hidden="true">
                      →
                    </span>
                  </span>
                )}
                <div className="dns-hop failed">
                  <span>{t(failure.key, failure.values)}</span>
                </div>
              </div>
            )}
          </div>

          {/* Colour is paired with text, never carrying the meaning alone (a11y). */}
          <div className="dns-outcome nd-muted">
            {t('overview.firstObserved')} {formatTimestamp(Date.parse(current.first_seen))} ·{' '}
            {t('overview.lastConfirmed')} {formatTimestamp(Date.parse(current.last_seen))}
          </div>
        </>
      )}

      <div className="dns-history">
        <div className="dns-history-head">{t('overview.resolutionHistory')}</div>
        <DataTable
          rows={history}
          columns={historyColumns}
          rowKey={(row) => String(row.id)}
          empty={t('overview.historyEmpty')}
          renderCard={(row) => (
            <div>
              <div className="nd-muted">{formatTimestamp(Date.parse(row.at))}</div>
              <div className="dns-history-summary">{chainSummary(row.chain)}</div>
            </div>
          )}
        />
      </div>
    </section>
  );
}
