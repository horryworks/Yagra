// Alert history (Alerts ▸ History). Append-only lifecycle log from /alerts/history. Each row
// is a transition (fire / clear). MTTR is open→clear in this model (§3.2: ack/response time is
// external and not measured here). Empty in skeleton mode (no persistent store). Rendered with
// the virtualized DataTable on the v2 table standard.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { alertWhat, formatTimestamp, severityColorVar, stateLabel } from '../lib/format';
import { api } from '../services/api';
import type { AlertHistoryRow } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Badge } from '../components/ui/Badge';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';

/** What fired: the metric and (for threshold checks) the condition it crossed. Liveness up/down
 *  reads as "Reachability"; legacy rows with no captured metric read as "—". Formatting logic is
 *  the pure `alertWhat` (unit-tested); this just maps its parts to styled spans. */
function WhatCell({ row }: { row: AlertHistoryRow }) {
  const what = alertWhat(row);
  if (what.kind === 'none') return <span className="muted">—</span>;
  if (what.kind === 'liveness') return <span>Reachability</span>;
  return (
    <span>
      <span className="mono">{what.metric}</span>
      {what.condition && <span className="muted"> {what.condition}</span>}
      {what.observed && <span className="muted"> ({what.observed})</span>}
    </span>
  );
}

const PAGE_SIZE = 100;

export function HistoryPage() {
  const [rows, setRows] = useState<AlertHistoryRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [exhausted, setExhausted] = useState(false);
  // Re-entrancy guard: DataTable fires onReachEnd on every render while the last row is in view,
  // so coalesce overlapping page loads into one in-flight request.
  const loadingMore = useRef(false);
  const { nodeName } = useEntityNames();

  useEffect(() => {
    api
      .listAlertHistory({ limit: PAGE_SIZE })
      .then((page) => {
        setRows(page);
        setExhausted(page.length < PAGE_SIZE);
      })
      .catch(() => undefined)
      .finally(() => setLoading(false));
  }, []);

  // Keyset "load older": fetch the next page strictly older than the last loaded row. The log is
  // append-only and can grow without bound, so we page on scroll instead of one capped fetch.
  const loadMore = useCallback(() => {
    if (loadingMore.current || exhausted) return;
    const last = rows[rows.length - 1];
    if (!last) return;
    loadingMore.current = true;
    api
      .listAlertHistory({ limit: PAGE_SIZE, before: last.recorded_at })
      .then((page) => {
        setRows((cur) => [...cur, ...page]);
        setExhausted(page.length < PAGE_SIZE);
      })
      .catch(() => undefined)
      .finally(() => {
        loadingMore.current = false;
      });
  }, [rows, exhausted]);

  // Columns close over `nodeName`, so rebuild them when the inventory resolves.
  const columns = useMemo<Column<AlertHistoryRow>[]>(
    () => [
      {
        key: 'sev',
        header: 'Severity',
        width: '110px',
        render: (r) => (
          <span className="yt-status">
            <span className="yt-status-dot" style={{ background: severityColorVar(r.severity) }} />
            <span className="muted">{r.severity}</span>
          </span>
        ),
      },
      {
        key: 'node',
        header: 'Node',
        width: '1.4fr',
        render: (r) => <EntityName name={nodeName(r.node)} id={r.node} />,
      },
      { key: 'what', header: 'What', width: '1.6fr', render: (r) => <WhatCell row={r} /> },
      { key: 'state', header: 'State', width: '120px', render: (r) => stateLabel(r.state) },
      {
        key: 'phase',
        header: 'Event',
        width: '100px',
        render: (r) =>
          r.resolved ? <Badge tone="up">cleared</Badge> : <Badge tone="critical">fired</Badge>,
      },
      {
        // Read-only ack mirrored from the external tool (ADR-015) — Yagra has no ack action.
        key: 'acked',
        header: 'Acked',
        width: '120px',
        render: (r) =>
          r.acked ? (
            <span
              className="muted"
              title={`Acknowledged in ${r.acked.source} by ${r.acked.by}${r.acked.note ? ` — ${r.acked.note}` : ''} (read-only, mirrored from your external tool)`}
            >
              {r.acked.source}
            </span>
          ) : (
            <span className="muted">—</span>
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
        title="History"
        trail={[{ label: 'Alerts' }, { label: 'History' }]}
        note="Append-only alert lifecycle: each row is a fire or clear transition."
      />
      <TableToolbar>
        <TableSpacer />
        <ResultCount shown={rows.length} noun={exhausted ? 'transitions' : 'transitions loaded'} />
      </TableToolbar>
      <DataTable
        rows={rows}
        columns={columns}
        rowKey={(r) => `${r.node}|${r.check}|${r.at_unix_ms}|${r.resolved}`}
        onReachEnd={loadMore}
        empty="No alert history yet."
        loading={loading}
      />
    </div>
  );
}
