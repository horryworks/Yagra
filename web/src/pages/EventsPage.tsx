// Events (Alerts ▸ Events). Append-only log of received passive events (syslog / SNMP traps /
// webhooks), keyset-paged newest-first. The rule-authoring surface: browse what devices actually
// send, then write rules against it. The node filter lives in the URL (?node_id=) so a deep-link
// from a node (NodeDetail ▸ Events "Open in Events →") lands here pre-filtered; the other filters
// (kind / matched / search+regex / time-range) stay local. Fetch/paging, columns, and the filter
// bar are shared with the NodeDetail Events tab via components/EventLog. Empty in skeleton mode.

import { useMemo, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { useEntityNames } from '../components/ui/EntityName';
import { DataTable } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { useEventLog } from '../components/EventLog/useEventLog';
import { eventColumns } from '../components/EventLog/eventColumns';
import {
  EventFilterBar,
  type KindFilter,
  type MatchedFilter,
} from '../components/EventLog/EventFilterBar';
import { readNodeIdParam, writeNodeIdParam } from '../components/EventLog/eventFilters';

export function EventsPage() {
  const { t } = useTranslation('alerts');
  const [searchParams, setSearchParams] = useSearchParams();
  const nodeId = readNodeIdParam(searchParams);
  const { nodeName } = useEntityNames();
  const [kind, setKind] = useState<KindFilter>('');
  const [matched, setMatched] = useState<MatchedFilter>('');
  const [search, setSearch] = useState('');
  const [regex, setRegex] = useState(false);
  const [start, setStart] = useState<string | undefined>(undefined);
  const [end, setEnd] = useState<string | undefined>(undefined);

  const { rows, loading, exhausted, loadMore } = useEventLog({
    kind: kind || undefined,
    node_id: nodeId ?? undefined,
    matched: matched === '' ? undefined : matched === 'matched',
    search: search || undefined,
    regex,
    start,
    end,
  });

  const columns = useMemo(() => eventColumns(nodeName, t), [nodeName, t]);

  const setNode = (node: { id: string; name: string } | null) => {
    const params = new URLSearchParams(searchParams);
    writeNodeIdParam(params, node?.id ?? null);
    setSearchParams(params, { replace: true });
  };

  return (
    <div className="page-fill">
      <PageHeader
        title={t('nav:alerts.events')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.events') }]}
        note={t('events.note')}
      />
      <TableToolbar>
        <EventFilterBar
          kind={kind}
          onKindChange={setKind}
          matched={matched}
          onMatchedChange={setMatched}
          regex={regex}
          onRegexChange={setRegex}
          onSearchChange={setSearch}
          onRangeChange={(s, e) => {
            setStart(s);
            setEnd(e);
          }}
          showNodePicker
          nodeId={nodeId}
          nodeLabel={nodeId ? nodeName(nodeId) : undefined}
          onNodeChange={setNode}
        />
        <TableSpacer />
        <ResultCount
          shown={rows.length}
          noun={exhausted ? t('events.events') : t('events.eventsLoaded')}
        />
      </TableToolbar>
      <DataTable
        rows={rows}
        columns={columns}
        rowKey={(r) => r.id}
        onReachEnd={loadMore}
        empty={t('events.empty')}
        loading={loading}
      />
    </div>
  );
}
