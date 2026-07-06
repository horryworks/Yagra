// Column set for the passive-event log (DataTable), shared by the Events page and the
// NodeDetail Events tab. `showSource` off drops the Source column — redundant inside a single
// node's tab, where every row is that node. Device-supplied text (message) is rendered as
// plain text only (no dangerouslySetInnerHTML), per the security rules.

import type { Column } from '../ui/DataTable';
import type { EventRow } from '../../types/api';
import { Badge } from '../ui/Badge';
import { EntityName } from '../ui/EntityName';
import { formatTimestamp } from '../../lib/format';
import './eventColumns.css';

const ACTION_TONE: Record<EventRow['action'], 'critical' | 'warning' | 'up' | 'neutral' | 'info'> = {
  fired: 'critical',
  refreshed: 'warning',
  cleared: 'up',
  suppressed: 'neutral',
  info: 'info',
  none: 'neutral',
};

/** Build the event-log columns. `nodeName` resolves a node_id → human name (from useEntityNames). */
export function eventColumns(
  nodeName: (id: string) => string,
  opts?: { showSource?: boolean },
): Column<EventRow>[] {
  const showSource = opts?.showSource ?? true;
  const cols: Column<EventRow>[] = [
    {
      key: 'kind',
      header: 'Kind',
      width: '90px',
      render: (r) => <Badge tone="neutral">{r.kind}</Badge>,
    },
  ];
  if (showSource) {
    cols.push({
      key: 'source',
      header: 'Source',
      width: '160px',
      render: (r) =>
        r.node_id ? (
          <EntityName name={nodeName(r.node_id)} id={r.node_id} />
        ) : (
          <span className="mono muted">{r.source_ip ?? '—'}</span>
        ),
    });
  }
  cols.push(
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
  );
  return cols;
}
