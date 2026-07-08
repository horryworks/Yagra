// Events (Alerts ▸ Events). Append-only log of received passive events (syslog / SNMP traps /
// webhooks), keyset-paged newest-first. The rule-authoring surface: browse what devices actually
// send, then write rules against it. The node filter lives in the URL (?node_id=) so a deep-link
// from a node (NodeDetail ▸ Events "Open in Events →") lands here pre-filtered; kind/matched stay
// local. Fetch/paging + columns are shared with the NodeDetail Events tab via components/EventLog.
// Empty in skeleton mode.

import { useEffect, useMemo, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import type { EventKind } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { useEntityNames } from '../components/ui/EntityName';
import { DataTable } from '../components/ui/DataTable';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { Select } from '../components/ui/Field';
import { NodePicker } from '../components/NodePicker/NodePicker';
import { useEventLog } from '../components/EventLog/useEventLog';
import { eventColumns } from '../components/EventLog/eventColumns';
import { readNodeIdParam, writeNodeIdParam } from '../components/EventLog/eventFilters';

type KindFilter = '' | EventKind;
type MatchedFilter = '' | 'matched' | 'unmatched';

export function EventsPage() {
  const { t } = useTranslation('alerts');
  const [searchParams, setSearchParams] = useSearchParams();
  const nodeId = readNodeIdParam(searchParams);
  const { nodeName } = useEntityNames();
  const [kind, setKind] = useState<KindFilter>('');
  const [matched, setMatched] = useState<MatchedFilter>('');
  const [search, setSearch] = useState('');
  const [debouncedSearch, setDebouncedSearch] = useState('');

  // Debounce the text box so we don't refetch on every keystroke (matches the MIB search).
  useEffect(() => {
    const t = setTimeout(() => setDebouncedSearch(search.trim()), 200);
    return () => clearTimeout(t);
  }, [search]);

  const { rows, loading, exhausted, loadMore } = useEventLog({
    kind: kind || undefined,
    node_id: nodeId ?? undefined,
    matched: matched === '' ? undefined : matched === 'matched',
    search: debouncedSearch || undefined,
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
        <Select value={kind} onChange={(e) => setKind(e.target.value as KindFilter)}>
          <option value="">{t('events.filters.allKinds')}</option>
          <option value="syslog">syslog</option>
          <option value="trap">trap</option>
          <option value="webhook">webhook</option>
        </Select>
        <Select value={matched} onChange={(e) => setMatched(e.target.value as MatchedFilter)}>
          <option value="">{t('events.filters.allEvents')}</option>
          <option value="matched">{t('events.filters.matched')}</option>
          <option value="unmatched">{t('events.filters.unmatched')}</option>
        </Select>
        <NodePicker
          value={nodeId}
          valueLabel={nodeId ? nodeName(nodeId) : undefined}
          onChange={setNode}
          placeholder={t('nav:nodes.all')}
        />
        <SearchInput
          value={search}
          onChange={setSearch}
          placeholder={t('events.searchPlaceholder')}
          ariaLabel={t('events.searchAria')}
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
