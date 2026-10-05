// SPDX-License-Identifier: AGPL-3.0-only
// The third section of Alerts ▸ Notification delivery: the delivery log (ADR-195). One row per
// notification sent to a channel, newest first, saying whether it arrived and - when it did not -
// whether Yagra, the network or the receiving service failed. A row opens in place to show every
// attempt and the start of what the receiving service answered.
//
// Server-side filters and keyset paging, like the audit log: the log grows with the fleet. The
// query logic is `deliveryLogQuery.ts`; this file is layout.

import { useCallback, useEffect, useMemo, useRef, useState, type Ref } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api, errMsg } from '../services/api';
import type { DeliveryRow, NotificationChannel } from '../types/api';
import { DataTable, type Column } from '../components/ui/DataTable';
import { ListToolbar } from '../components/ui/ListToolbar';
import { Badge } from '../components/ui/Badge';
import { Button } from '../components/ui/Button';
import { EntityName } from '../components/ui/EntityName';
import { useEntityNames } from '../components/ui/entityNames';
import { TimeCell } from '../components/ui/tableCells';
import { serverToolbarFilters } from '../lib/listToolbar';
import { isAnyFiltered, specColumns, type FilterState } from '../lib/columnFilter';
import { SEVERITY_TONE, severityLabel } from '../lib/format';
import {
  appendPage,
  channelLabel,
  deliveryFilters,
  durationText,
  nextCursor,
  queryFor,
  sideExplanationKey,
  subjectText,
  type DeliveryCursor,
} from './deliveryLogQuery';

/** The filter state the page holds for this table (lifted so a channel row can narrow it). */
export interface DeliveryLogFilters {
  filters: FilterState;
  setFilters: (next: FilterState) => void;
  nowMs: number;
}

function columnsFor(
  t: TFunction,
  specs: ReturnType<typeof deliveryFilters>,
  nodeName: (id: string) => string,
): Column<DeliveryRow>[] {
  const cols: Column<DeliveryRow>[] = [
    {
      key: 'range',
      header: t('routing.log.cols.time'),
      width: '170px',
      render: (r) => <TimeCell iso={r.at} />,
    },
    {
      key: 'channel',
      header: t('routing.log.cols.channel'),
      width: '1fr',
      render: (r) => {
        const label = channelLabel(t, r);
        return (
          <span className="ellipsis" title={label}>
            {label}
          </span>
        );
      },
    },
    {
      key: 'subject',
      header: t('routing.log.cols.subject'),
      width: '1.2fr',
      render: (r) =>
        r.node_id ? (
          <EntityName name={nodeName(r.node_id)} id={r.node_id} />
        ) : (
          <span className="ellipsis" title={subjectText(t, r)}>
            {subjectText(t, r)}
          </span>
        ),
    },
    {
      key: 'event',
      header: t('routing.log.cols.event'),
      width: '120px',
      render: (r) => <Badge tone="neutral">{t(`routing.log.event.${r.event}`)}</Badge>,
    },
    {
      key: 'result',
      header: t('routing.log.cols.result'),
      width: '110px',
      render: (r) => (
        <Badge tone={r.result === 'delivered' ? 'up' : r.result === 'failed' ? 'critical' : 'neutral'}>
          {t(`routing.log.result.${r.result}`)}
        </Badge>
      ),
    },
    {
      key: 'side',
      header: t('routing.log.cols.side'),
      width: '150px',
      render: (r) =>
        r.side ? (
          <span title={t(sideExplanationKey(r.side))}>{t(`routing.log.side.${r.side}`)}</span>
        ) : (
          <span className="muted">—</span>
        ),
    },
    {
      key: 'status',
      header: t('routing.log.cols.status'),
      width: '80px',
      render: (r) => (r.status != null ? <span className="mono">{r.status}</span> : <span className="muted">—</span>),
    },
    {
      key: 'attempts',
      header: t('routing.log.cols.attempts'),
      width: '70px',
      align: 'right',
      render: (r) => <span className="mono">{r.attempts}</span>,
    },
    {
      key: 'duration',
      header: t('routing.log.cols.duration'),
      width: '90px',
      align: 'right',
      render: (r) => <span className="mono">{durationText(r.duration_ms)}</span>,
    },
  ];
  for (const c of cols) c.filter = specs[c.key];
  return cols;
}

/** What a row shows when it is opened: the verdict, every attempt, and what the remote answered. */
function DeliveryDetail({ row }: { row: DeliveryRow }) {
  const { t } = useTranslation('alertsConfig');
  return (
    <div className="routing-log-detail">
      {row.side && <p className="routing-log-verdict">{t(sideExplanationKey(row.side))}</p>}
      {row.severity && (
        <p className="routing-log-line">
          {t('routing.log.detail.severity')}{' '}
          <Badge tone={SEVERITY_TONE[row.severity]}>{severityLabel(row.severity)}</Badge>
        </p>
      )}
      {row.error && (
        <p className="routing-log-line">
          {t('routing.log.detail.error')} <span className="mono">{row.error}</span>
        </p>
      )}
      {row.response && (
        <div className="routing-log-line">
          {t('routing.log.detail.response')}
          <pre className="routing-log-response mono">{row.response}</pre>
        </div>
      )}
      <ol className="routing-log-attempts">
        {row.attempt_log.map((a, i) => (
          <li key={i}>
            <span className="mono">{durationText(a.duration_ms)}</span>{' '}
            {a.side ? (
              <>
                {t(`routing.log.side.${a.side}`)}
                {a.status != null && <span className="mono"> {a.status}</span>}
                {a.error && <span className="muted"> — {a.error}</span>}
              </>
            ) : (
              t('routing.log.detail.attemptOk')
            )}
          </li>
        ))}
      </ol>
    </div>
  );
}

export function DeliveryLog({
  channels,
  filterState,
  sectionRef,
}: {
  channels: NotificationChannel[];
  filterState: DeliveryLogFilters;
  sectionRef?: Ref<HTMLElement>;
}) {
  const { t } = useTranslation('alertsConfig');
  const { nodeName } = useEntityNames();
  const { filters, setFilters, nowMs } = filterState;
  const [rows, setRows] = useState<DeliveryRow[]>([]);
  const [cursor, setCursor] = useState<DeliveryCursor | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  // Refresh re-reads the log without touching the URL: writing the same filters back is the same
  // query string, which `useSearchParams` hands back as the same object, so nothing would refetch.
  const [reloadNonce, setReloadNonce] = useState(0);
  // DataTable fires onReachEnd on every render while the last row is in view; one request at a time.
  const loadingMore = useRef(false);

  // Specs depend on the channel list (its names are the filter's options); the columns also on the
  // name resolver. The fetch reads the specs only, so a name resolving does not refetch the log.
  const specs = useMemo(() => deliveryFilters(t, channels), [t, channels]);
  const filterCols = useMemo(() => specColumns(specs), [specs]);
  const columns = useMemo(() => columnsFor(t, specs, nodeName), [t, specs, nodeName]);
  const filtered = isAnyFiltered(filterCols, filters);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .listNotificationDeliveries(queryFor(filterCols, filters, null, nowMs))
      .then((page) => {
        if (cancelled) return;
        setRows(page);
        setCursor(nextCursor(page));
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(errMsg(e, t('routing.log.err.load')));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [filterCols, filters, nowMs, t, reloadNonce]);

  const loadMore = useCallback(() => {
    if (loadingMore.current || cursor === null) return;
    loadingMore.current = true;
    api
      .listNotificationDeliveries(queryFor(filterCols, filters, cursor, nowMs))
      .then((page) => {
        setRows((cur) => appendPage(cur, page));
        setCursor(nextCursor(page));
      })
      .catch((e: unknown) => setError(errMsg(e, t('routing.log.err.load'))))
      .finally(() => {
        loadingMore.current = false;
      });
  }, [cursor, filterCols, filters, nowMs, t]);

  const reload = () => setReloadNonce((n) => n + 1);

  return (
    <section className="routing-log-section" ref={sectionRef}>
      <ListToolbar
        list={serverToolbarFilters(filterCols, { filters, setFilters })}
        labels={{
          range: t('routing.log.cols.time'),
          channel: t('routing.log.cols.channel'),
          event: t('routing.log.cols.event'),
          result: t('routing.log.cols.result'),
          side: t('routing.log.cols.side'),
        }}
        count={{ shown: rows.length, noun: (n) => t('routing.log.noun', { count: n }) }}
        leading={<h2 className="table-section-title">{t('routing.log.title')}</h2>}
      >
        <Button variant="outline" onClick={reload}>
          {t('routing.log.refresh')}
        </Button>
      </ListToolbar>
      {error && <p className="form-error">{error}</p>}
      <div className="routing-log-table">
        <DataTable
          tableId="settings.notificationDeliveries"
          rows={rows}
          columns={columns}
          filters={filters}
          onFiltersChange={setFilters}
          rowKey={(r) => String(r.id)}
          onRowClick={(r) => setOpen((cur) => (cur === String(r.id) ? null : String(r.id)))}
          expanded={(r) => (open === String(r.id) ? <DeliveryDetail row={r} /> : null)}
          expandedKey={open}
          onReachEnd={cursor === null ? undefined : loadMore}
          loading={loading}
          empty={filtered ? t('common:filter.noMatch') : t('routing.log.empty')}
        />
      </div>
    </section>
  );
}
