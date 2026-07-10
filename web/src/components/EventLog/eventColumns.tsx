// Column set for the passive-event log (DataTable), shared by the Events page and the
// NodeDetail Events tab. `showSource` off drops the Source column — redundant inside a single
// node's tab, where every row is that node. Device-supplied text (message) is rendered as
// plain text only (no dangerouslySetInnerHTML), per the security rules.

import type { TFunction } from 'i18next';
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

/** Build the event-log columns. `nodeName` resolves a node_id → human name (from useEntityNames);
 *  `t` is the i18next translator from the calling component (rebuild the memo on language change so
 *  headers/labels re-resolve). Keys are namespace-qualified so any bound `t` resolves them. */
export function eventColumns(
  nodeName: (id: string) => string,
  t: TFunction,
  opts?: { showSource?: boolean },
): Column<EventRow>[] {
  const showSource = opts?.showSource ?? true;
  const cols: Column<EventRow>[] = [
    {
      key: 'kind',
      header: t('alerts:eventLog.cols.kind'),
      width: '90px',
      render: (r) => <Badge tone="neutral">{r.kind}</Badge>,
    },
  ];
  if (showSource) {
    cols.push({
      key: 'source',
      header: t('alerts:eventLog.cols.source'),
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
      header: t('alerts:eventLog.cols.message'),
      width: '2fr',
      render: (r) => (
        <span className="mono events-msg" title={r.message}>
          {r.message}
        </span>
      ),
    },
    {
      key: 'action',
      header: t('alerts:eventLog.cols.result'),
      width: '110px',
      render: (r) =>
        r.action === 'none' ? (
          <span className="muted">—</span>
        ) : (
          <Badge tone={ACTION_TONE[r.action]}>{t(`alerts:eventLog.action.${r.action}`)}</Badge>
        ),
    },
    {
      key: 'at',
      header: t('alerts:eventLog.cols.when'),
      width: '1fr',
      render: (r) => <span className="muted">{formatTimestamp(r.at_unix_ms)}</span>,
    },
  );
  return cols;
}
