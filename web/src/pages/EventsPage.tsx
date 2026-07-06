// Events (Alerts ▸ Events). Append-only log of received passive events (syslog / SNMP
// traps / webhooks), keyset-paged newest-first. The rule-authoring surface: browse what
// devices actually send, then write rules against it. Empty in skeleton mode.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { formatTimestamp } from '../lib/format';
import { api } from '../services/api';
import type { EventKind, EventRow } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Badge } from '../components/ui/Badge';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { Select } from '../components/ui/Field';
import './EventsPage.css';

const PAGE_SIZE = 100;

const ACTION_TONE: Record<EventRow['action'], 'critical' | 'warning' | 'up' | 'neutral' | 'info'> = {
  fired: 'critical',
  refreshed: 'warning',
  cleared: 'up',
  suppressed: 'neutral',
  info: 'info',
  none: 'neutral',
};

type KindFilter = '' | EventKind;
type MatchedFilter = '' | 'matched' | 'unmatched';

export function EventsPage() {
  const [rows, setRows] = useState<EventRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [exhausted, setExhausted] = useState(false);
  const [kind, setKind] = useState<KindFilter>('');
  const [matched, setMatched] = useState<MatchedFilter>('');
  const loadingMore = useRef(false);
  const { nodeName } = useEntityNames();

  const filterOpts = useCallback(
    (before?: string) => ({
      limit: PAGE_SIZE,
      ...(before ? { before } : {}),
      ...(kind ? { kind } : {}),
      ...(matched ? { matched: matched === 'matched' } : {}),
    }),
    [kind, matched],
  );

  // Reload from the top whenever a filter changes.
  useEffect(() => {
    setLoading(true);
    api
      .listEvents(filterOpts())
      .then((page) => {
        setRows(page);
        setExhausted(page.length < PAGE_SIZE);
      })
      .catch(() => undefined)
      .finally(() => setLoading(false));
  }, [filterOpts]);

  const loadMore = useCallback(() => {
    if (loadingMore.current || exhausted) return;
    const last = rows[rows.length - 1];
    if (!last) return;
    loadingMore.current = true;
    api
      .listEvents(filterOpts(last.recorded_at))
      .then((page) => {
        setRows((cur) => [...cur, ...page]);
        setExhausted(page.length < PAGE_SIZE);
      })
      .catch(() => undefined)
      .finally(() => {
        loadingMore.current = false;
      });
  }, [rows, exhausted, filterOpts]);

  const columns = useMemo<Column<EventRow>[]>(
    () => [
      {
        key: 'kind',
        header: 'Kind',
        width: '90px',
        render: (r) => <Badge tone="neutral">{r.kind}</Badge>,
      },
      {
        key: 'source',
        header: 'Source',
        width: '160px',
        render: (r) =>
          r.node_id ? (
            <EntityName name={nodeName(r.node_id)} id={r.node_id} />
          ) : (
            <span className="mono muted">{r.source_ip ?? '—'}</span>
          ),
      },
      {
        key: 'message',
        header: 'Message',
        width: '2fr',
        render: (r) => (
          <span className="mono events-msg" title={r.message}>
            {r.message}
          </span>
        ),
      },
      {
        key: 'action',
        header: 'Result',
        width: '110px',
        render: (r) =>
          r.action === 'none' ? (
            <span className="muted">—</span>
          ) : (
            <Badge tone={ACTION_TONE[r.action]}>{r.action}</Badge>
          ),
      },
      {
        key: 'at',
        header: 'When',
        width: '1fr',
        render: (r) => <span className="muted">{formatTimestamp(r.at_unix_ms)}</span>,
      },
    ],
    [nodeName],
  );

  return (
    <div className="page-fill">
      <PageHeader
        title="Events"
        trail={[{ label: 'Alerts' }, { label: 'Events' }]}
        note="Received syslog / SNMP-trap / webhook events. Unmatched events are kept for 24h to help author rules; matched events follow the alert-history retention."
      />
      <TableToolbar>
        <Select value={kind} onChange={(e) => setKind(e.target.value as KindFilter)}>
          <option value="">All kinds</option>
          <option value="syslog">syslog</option>
          <option value="trap">trap</option>
          <option value="webhook">webhook</option>
        </Select>
        <Select value={matched} onChange={(e) => setMatched(e.target.value as MatchedFilter)}>
          <option value="">All events</option>
          <option value="matched">Matched a rule</option>
          <option value="unmatched">Unmatched</option>
        </Select>
        <TableSpacer />
        <ResultCount shown={rows.length} noun={exhausted ? 'events' : 'events loaded'} />
      </TableToolbar>
      <DataTable
        rows={rows}
        columns={columns}
        rowKey={(r) => r.id}
        onReachEnd={loadMore}
        empty="No events received yet."
        loading={loading}
      />
    </div>
  );
}
