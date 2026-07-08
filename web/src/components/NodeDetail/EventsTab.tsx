// NodeDetail ▸ Events tab: this node's received passive events (syslog / SNMP traps / webhooks) as
// a full keyset-paged log, reusing the shared event-log hook + columns. The Source column is
// dropped (every row is this node). "Open in Events →" deep-links to the node-filtered Events page.

import { useMemo } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import type { NodeDetail } from '../../types/api';
import { DataTable } from '../ui/DataTable';
import { ResultCount } from '../ui/TableToolbar';
import { useEntityNames } from '../ui/EntityName';
import { useEventLog } from '../EventLog/useEventLog';
import { eventColumns } from '../EventLog/eventColumns';

export function EventsTab({ node }: { node: NodeDetail }) {
  const { t } = useTranslation('alerts');
  const { nodeName } = useEntityNames();
  const { rows, loading, exhausted, loadMore } = useEventLog({ node_id: node.id });
  const columns = useMemo(
    () => eventColumns(nodeName, t, { showSource: false }),
    [nodeName, t],
  );

  return (
    <div className="nd-ev">
      <div className="nd-ev-head">
        <Link className="nd-ev-open" to={`/alerts/events?node_id=${encodeURIComponent(node.id)}`}>
          {t('eventLog.openInEvents')} →
        </Link>
        <ResultCount
          shown={rows.length}
          noun={exhausted ? t('events.events') : t('events.eventsLoaded')}
        />
      </div>
      <DataTable
        rows={rows}
        columns={columns}
        rowKey={(r) => r.id}
        onReachEnd={loadMore}
        empty={t('eventLog.emptyNode')}
        loading={loading}
      />
    </div>
  );
}
