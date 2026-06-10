// Alert history (Alerts ▸ History). Append-only lifecycle log from /alerts/history. Each row
// is a transition (fire / clear). MTTR is open→clear in this model (§3.2: ack/response time is
// external and not measured here). Empty in skeleton mode (no persistent store).

import { useEffect, useState } from 'react';
import { formatTimestamp, severityColorVar, stateLabel } from '../lib/format';
import { api } from '../services/api';
import type { AlertHistoryRow } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Badge } from '../components/ui/Badge';
import { DataTable, type Column } from '../components/ui/DataTable';

const COLUMNS: Column<AlertHistoryRow>[] = [
  {
    key: 'sev',
    header: 'Severity',
    width: '90px',
    render: (r) => (
      <span className="statusdot-wrap">
        <span
          className="statusdot"
          style={{ background: severityColorVar(r.severity), width: 9, height: 9, borderRadius: 9 }}
        />
        <span className="muted">{r.severity}</span>
      </span>
    ),
  },
  { key: 'node', header: 'Node', width: '1.4fr', render: (r) => <span className="mono">{r.node}</span> },
  { key: 'state', header: 'State', width: '110px', render: (r) => stateLabel(r.state) },
  {
    key: 'phase',
    header: 'Event',
    width: '90px',
    render: (r) =>
      r.resolved ? <Badge tone="up">cleared</Badge> : <Badge tone="critical">fired</Badge>,
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

  useEffect(() => {
    api
      .listAlertHistory(200)
      .then(setRows)
      .catch(() => undefined);
  }, []);

  return (
    <div className="page-fill">
      <PageHeader
        title="History"
        trail={[{ label: 'Alerts' }, { label: 'History' }]}
        note={`${rows.length} recent transitions`}
      />
      <Card className="page-fill-card">
        <DataTable
          rows={rows}
          columns={COLUMNS}
          rowKey={(r) => `${r.node}|${r.check}|${r.at_unix_ms}|${r.resolved}`}
          empty="No alert history yet."
        />
      </Card>
    </div>
  );
}
