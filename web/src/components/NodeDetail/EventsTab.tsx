// NodeDetail ▸ Events tab: this node's received passive events (syslog / SNMP traps / webhooks) as
// a full keyset-paged log, reusing the shared event-log hook + columns + filter bar. The Source
// column is dropped (every row is this node) and the node picker is hidden (node is fixed); the
// kind / matched / search+regex / time-range filters are local. "Open in Events →" deep-links to
// the node-filtered Events page.

import { useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import type { NodeDetail } from '../../types/api';
import { DataTable } from '../ui/DataTable';
import { TableToolbar, ResultCount } from '../ui/TableToolbar';
import { useEntityNames } from '../ui/EntityName';
import { useEventLog } from '../EventLog/useEventLog';
import { eventColumns } from '../EventLog/eventColumns';
import {
  EventFilterBar,
  type KindFilter,
  type MatchedFilter,
} from '../EventLog/EventFilterBar';

export function EventsTab({ node }: { node: NodeDetail }) {
  const { t } = useTranslation('alerts');
  const { nodeName } = useEntityNames();
  const [kind, setKind] = useState<KindFilter>('');
  const [matched, setMatched] = useState<MatchedFilter>('');
  const [search, setSearch] = useState('');
  const [regex, setRegex] = useState(false);
  const [start, setStart] = useState<string | undefined>(undefined);
  const [end, setEnd] = useState<string | undefined>(undefined);

  const { rows, loading, exhausted, loadMore } = useEventLog({
    node_id: node.id,
    kind: kind || undefined,
    matched: matched === '' ? undefined : matched === 'matched',
    search: search || undefined,
    regex,
    start,
    end,
  });
  const columns = useMemo(() => eventColumns(nodeName, t, { showSource: false }), [nodeName, t]);

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
        />
      </TableToolbar>
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
