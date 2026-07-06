// NodeDetail ▸ Events tab: this node's received passive events (syslog / SNMP traps / webhooks) as
// a full keyset-paged log, reusing the shared event-log hook + columns. The Source column is
// dropped (every row is this node). "Open in Events →" deep-links to the node-filtered Events page.

import { useMemo } from 'react';
import { Link } from 'react-router-dom';
import type { NodeDetail } from '../../types/api';
import { DataTable } from '../ui/DataTable';
import { ResultCount } from '../ui/TableToolbar';
import { useEntityNames } from '../ui/EntityName';
import { useEventLog } from '../EventLog/useEventLog';
import { eventColumns } from '../EventLog/eventColumns';

export function EventsTab({ node }: { node: NodeDetail }) {
  const { nodeName } = useEntityNames();
  const { rows, loading, exhausted, loadMore } = useEventLog({ node_id: node.id });
  const columns = useMemo(() => eventColumns(nodeName, { showSource: false }), [nodeName]);

  return (
    <div className="nd-ev">
      <div className="nd-ev-head">
        <Link className="nd-ev-open" to={`/alerts/events?node_id=${encodeURIComponent(node.id)}`}>
          Open in Events →
        </Link>
        <ResultCount shown={rows.length} noun={exhausted ? 'events' : 'events loaded'} />
      </div>
      <DataTable
        rows={rows}
        columns={columns}
        rowKey={(r) => r.id}
        onReachEnd={loadMore}
        empty="No events received from this node yet."
        loading={loading}
      />
    </div>
  );
}
