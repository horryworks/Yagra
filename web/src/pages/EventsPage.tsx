// Events (Alerts ▸ Events). Append-only log of received passive events (syslog / SNMP traps /
// webhooks), keyset-paged newest-first. The rule-authoring surface: browse what devices actually
// send, then write rules against it. The node filter lives in the URL (?node_id=) so a deep-link
// from a node (NodeDetail ▸ Events "Open in Events →") lands here pre-filtered; kind/matched stay
// local. Fetch/paging + columns are shared with the NodeDetail Events tab via components/EventLog.
// Empty in skeleton mode.

import { useMemo, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import type { EventKind } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { useEntityNames } from '../components/ui/EntityName';
import { DataTable } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { Select } from '../components/ui/Field';
import { NodePicker } from '../components/NodePicker/NodePicker';
import { useEventLog } from '../components/EventLog/useEventLog';
import { eventColumns } from '../components/EventLog/eventColumns';
import { readNodeIdParam, writeNodeIdParam } from '../components/EventLog/eventFilters';

type KindFilter = '' | EventKind;
type MatchedFilter = '' | 'matched' | 'unmatched';

export function EventsPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const nodeId = readNodeIdParam(searchParams);
  const { nodeName } = useEntityNames();
  const [kind, setKind] = useState<KindFilter>('');
  const [matched, setMatched] = useState<MatchedFilter>('');

  const { rows, loading, exhausted, loadMore } = useEventLog({
    kind: kind || undefined,
    node_id: nodeId ?? undefined,
    matched: matched === '' ? undefined : matched === 'matched',
  });

  const columns = useMemo(() => eventColumns(nodeName), [nodeName]);

  const setNode = (node: { id: string; name: string } | null) => {
    const params = new URLSearchParams(searchParams);
    writeNodeIdParam(params, node?.id ?? null);
    setSearchParams(params, { replace: true });
  };

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
        <NodePicker
          value={nodeId}
          valueLabel={nodeId ? nodeName(nodeId) : undefined}
          onChange={setNode}
          placeholder="All nodes"
        />
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
