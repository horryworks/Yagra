// Alert history (Alerts ▸ History). Append-only lifecycle log from /alerts/history. Each row
// is a transition (fire / clear). MTTR is open→clear in this model (§3.2: ack/response time is
// external and not measured here). Empty in skeleton mode (no persistent store). Rendered with
// the virtualized DataTable on the v2 table standard.

import { useEffect, useState } from 'react';
import { formatTimestamp, severityColorVar, stateLabel } from '../lib/format';
import { api } from '../services/api';
import type { AlertHistoryRow } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Badge } from '../components/ui/Badge';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';

const COLUMNS: Column<AlertHistoryRow>[] = [
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
  { key: 'node', header: 'Node', width: '1.4fr', render: (r) => <span className="mono">{r.node}</span> },
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
];

export function HistoryPage() {
  const [rows, setRows] = useState<AlertHistoryRow[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    api
      .listAlertHistory(200)
      .then(setRows)
      .catch(() => undefined)
      .finally(() => setLoading(false));
  }, []);

  return (
    <div className="page-fill">
      <PageHeader
        title="History"
        trail={[{ label: 'Alerts' }, { label: 'History' }]}
        note="Append-only alert lifecycle: each row is a fire or clear transition."
      />
      <TableToolbar>
        <TableSpacer />
        <ResultCount shown={rows.length} noun="recent transitions" />
      </TableToolbar>
      <DataTable
        rows={rows}
        columns={COLUMNS}
        rowKey={(r) => `${r.node}|${r.check}|${r.at_unix_ms}|${r.resolved}`}
        empty="No alert history yet."
        loading={loading}
      />
    </div>
  );
}
