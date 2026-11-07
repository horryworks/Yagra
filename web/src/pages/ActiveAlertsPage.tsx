// SPDX-License-Identifier: AGPL-3.0-only
// Active alerts — triage only (§3.2). Per the responsibility split, Yagra detects/correlates/
// suppresses/routes; acknowledgement, escalation and on-call live in external tools
// (PagerDuty/JSM). So there is NO Ack button, and Yagra does not track acknowledgement at all —
// not even as state mirrored back from the external tool (ADR-233).
//
// One per-alert action, and it appears only where it would actually work — a control that is
// permanently disabled is a promise the UI cannot keep: Explain (ADR-029) needs a configured
// provider AND a role that can spend the call. Silencing alerts is a maintenance window, never a
// per-alert action (ADR-234).
//
// Since ADR-243 the screen is the shared `DataTable` with Alerts ▸ History's columns, funnels and
// scope picker, so the two alert screens read alike. What is different is underneath: the rows
// arrive live over SSE and are all in the browser, so filtering, sorting and the scope are decided
// here (`activeAlertFilters.ts` says why that is allowed).

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { sortedAlerts, useAlertStore, useCan } from '../store';
import { api } from '../services/api';
import { alertRowKey, subjectNodeId } from '../lib/alertSubject';
import { alertNotes } from '../lib/alertNotes';
import { SEVERITY_ORDER } from '../lib/nodeState';
import { alertWhatOf, formatTimestamp, severityColorVar, severityLabel, stateLabel } from '../lib/format';
import { SEVERITIES, type Alert } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { DataTable, type Column } from '../components/ui/DataTable';
import { ListToolbar } from '../components/ui/ListToolbar';
import { columnLabels, serverToolbarFilters } from '../lib/listToolbar';
import { isAnyFiltered, specColumns } from '../lib/columnFilter';
import { applyFilters } from '../lib/filterPredicate';
import { facetCounts } from '../lib/filterCounts';
import { useFilterParams } from '../lib/useFilterParams';
import { useClientSort } from '../lib/useClientSort';
import { useEntityNames } from '../components/ui/entityNames';
import { subtreeGroupIds } from '../lib/nodeTree';
import { RcaModal } from '../components/Rca/RcaModal';
import { EmptyState } from '../components/ui/EmptyState';
import { AlertSubjectName } from '../widgets/AlertSubjectName';
import { AlertWhatText } from '../widgets/AlertWhatText';
import { AlertNotes } from '../widgets/AlertNotes';
import { useHeldNotifications } from '../widgets/useHeldNotifications';
import { heldFireFor } from './deliveryHours';
import { useFleetSummary } from '../dashboard/useFleetSummary';
import { useFleetWatch } from '../hooks/useFleetWatch';
import { isSilenced, watchChips } from '../lib/watchCounts';
import { WatchChips } from '../widgets/WatchChips';
import { addNodeHref } from '../lib/treeSelection';
import { ScopePicker } from '../components/ScopePicker/ScopePicker';
import {
  allScope,
  groupScopeLabel,
  nodeScopeLabel,
  type ScopeValue,
} from '../components/ScopePicker/scope';
import { readScope, scopeFilter, writeScope } from '../troubleshoot/findingsQuery';
import { activeAlertFilters, scopePredicate } from './activeAlertFilters';
import './ActiveAlertsPage.css';

export function ActiveAlertsPage() {
  const { t } = useTranslation('alerts');
  const [rcaEnabled, setRcaEnabled] = useState(false);
  const [explaining, setExplaining] = useState<{ node: string; check: string } | null>(null);

  const alerts = useAlertStore((s) => s.alerts);
  // Worst first, then newest — the order the table opens in and returns to when a sort is cleared.
  const all = useMemo(() => sortedAlerts(alerts), [alerts]);
  const { nodeName, nodeGroup, groups, groupName } = useEntityNames();
  const held = useHeldNotifications();

  // One answer per row to "which notes", shared by the cell, the Notes filter and its sort.
  const notesOf = useCallback(
    (a: Alert) => alertNotes(a, held.length === 0 ? null : heldFireFor(a, held, subjectNodeId(a))),
    [held],
  );
  const noteKindsOf = useCallback((a: Alert) => notesOf(a).map((n) => n.kind), [notesOf]);

  // The filters live in the URL under History's keys, so a narrowed triage view survives a reload
  // and can be pasted to whoever is looking at the same incident.
  const specs = useMemo(
    () => activeAlertFilters<Alert>(t, nodeName, noteKindsOf, SEVERITIES, SEVERITY_ORDER),
    [t, nodeName, noteKindsOf],
  );
  const filterCols = useMemo(() => specColumns(specs), [specs]);
  const { filters, setFilters } = useFilterParams(filterCols);

  // The scope rides beside the columns under History's `node_id` / `group_id`, and every change to
  // it is ONE write to the query string, through `setFilters`' `also` callback.
  const [params, setParams] = useSearchParams();
  const scopeIds = useMemo(() => readScope(params), [params]);
  const [scope, setScope] = useState<ScopeValue>(() => {
    if (scopeIds.nodeId) return { kind: 'node', id: scopeIds.nodeId, label: scopeIds.nodeId };
    if (scopeIds.groupId) return { kind: 'group', id: scopeIds.groupId, label: scopeIds.groupId };
    return allScope(t);
  });
  // A deep link arrives with an id and no label; name it once the inventory has.
  const scopeLabel =
    scope.kind === 'node' && scope.id === scope.label
      ? nodeScopeLabel(nodeName(scope.id), t)
      : scope.kind === 'group' && scope.id === scope.label
        ? groupScopeLabel(groupName(scope.id), t)
        : scope.label;
  const onScope = (v: ScopeValue) => {
    setScope(v);
    setFilters(filters, writeScope(scopeFilter(v)));
  };
  const subtree = useMemo(
    () => new Set(scopeIds.groupId ? subtreeGroupIds(groups, scopeIds.groupId) : []),
    [groups, scopeIds.groupId],
  );

  // What this list is not seeing (ADR-227). `?silenced=1` narrows it to the silenced rows; it is in
  // the URL beside the other filters so a link to "what are we holding back" can be pasted.
  const watch = useFleetWatch();
  const onlySilenced = params.get('silenced') === '1';
  const toggleSilenced = useCallback(() => {
    const p = new URLSearchParams(params);
    if (onlySilenced) p.delete('silenced');
    else p.set('silenced', '1');
    setParams(p, { replace: true });
  }, [onlySilenced, params, setParams]);
  const silenced = useMemo(() => all.filter(isSilenced).length, [all]);

  // Scope and the silenced chip first, then the columns: the facet counts are over what the scope
  // leaves, so "critical 3" means three in this folder, not three in the fleet.
  const scoped = useMemo(() => {
    const inScope = scopePredicate(scopeIds, subtree, nodeGroup);
    let rows = inScope ? all.filter(inScope) : all;
    if (onlySilenced) rows = rows.filter(isSilenced);
    return rows;
  }, [all, scopeIds, subtree, nodeGroup, onlySilenced]);
  const shown = useMemo(() => applyFilters(scoped, filterCols, filters, Date.now()), [scoped, filterCols, filters]);
  const counts = useMemo(
    () =>
      Object.fromEntries(
        filterCols
          .filter((c) => c.filter.kind === 'enum')
          .map((c) => [c.key, facetCounts(scoped, filterCols, filters, c.key, Date.now())]),
      ),
    [scoped, filterCols, filters],
  );

  // `rca_enabled` is the server's own answer to "would this button work" — an installation with no
  // provider would 503, so the affordance simply isn't offered there.
  useEffect(() => {
    let cancelled = false;
    api
      .getConfig()
      .then((cfg) => !cancelled && setRcaEnabled(cfg.rca_enabled === true))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);
  // Explain is AckAlerts-gated server-side — a caller without it would get a 403, so don't show a
  // button that only ever fails. It additionally needs a provider.
  const canRunRca = useCan('ack_alerts');
  const canConfig = useCan('manage_config');
  const canExplain = rcaEnabled && canRunRca;
  const navigate = useNavigate();

  const columns = useMemo<Column<Alert>[]>(() => {
    const cols: Column<Alert>[] = [
      {
        key: 'severity',
        header: t('history.cols.severity'),
        width: '110px',
        render: (a) => (
          <span className="yt-status">
            <span className="yt-status-dot" style={{ background: severityColorVar(a.severity) }} />
            <span className="muted">{severityLabel(a.severity)}</span>
          </span>
        ),
      },
      {
        key: 'node_q',
        header: t('history.cols.node'),
        width: '1.4fr',
        render: (a) => <AlertSubjectName alert={a} nodeName={nodeName} />,
      },
      {
        key: 'metric',
        header: t('history.cols.what'),
        width: '1.6fr',
        // What the check measured, in place of the check's (unnameable) hash id; the id stays on
        // hover because it is the handle the API takes (RCA).
        render: (a) => (
          <span title={a.check}>
            <AlertWhatText what={alertWhatOf(a)} />
          </span>
        ),
      },
      {
        key: 'state',
        header: t('history.cols.state'),
        width: '120px',
        render: (a) => stateLabel(a.state),
      },
      {
        key: 'notes',
        header: t('active.cols.notes'),
        width: '1.3fr',
        render: (a) => <AlertNotes stacked nodeName={nodeName} notes={notesOf(a)} />,
      },
      {
        key: 'range',
        header: t('history.cols.when'),
        width: '180px',
        render: (a) => {
          const when = formatTimestamp(a.at_unix_ms);
          return (
            <span className="muted" title={when}>
              {when}
            </span>
          );
        },
      },
    ];
    if (canExplain) {
      cols.push({
        key: 'actions',
        header: t('active.cols.actions'),
        // Wide enough for the ghost button's label in both languages; 90px cut "Explain" by 6px.
        width: '110px',
        align: 'right',
        // RCA takes a node id, so a pool-coverage alert gets no button rather than one that only
        // fails.
        render: (a) => {
          const node = subjectNodeId(a);
          if (node === null) return null;
          return (
            <span className="ytable-actions">
              <Button
                variant="ghost"
                className="active-alerts-explain"
                aria-label={t('rca:actionHint')}
                title={t('rca:actionHint')}
                onClick={() => setExplaining({ node, check: a.check })}
              >
                {t('rca:action')}
              </Button>
            </span>
          );
        },
      });
    }
    for (const c of cols) c.filter = specs[c.key];
    return cols;
  }, [t, nodeName, notesOf, canExplain, specs]);

  const sorting = useClientSort(columns, shown);

  // No alerts on an empty fleet is not "all clear" (ADR-209 decision 4): nothing is being watched.
  const { summary } = useFleetSummary();
  const noNodes = summary?.total === 0;
  const scopeActive = !!scopeIds.nodeId || !!scopeIds.groupId;
  const filtered = isAnyFiltered(filterCols, filters) || scopeActive || onlySilenced;

  return (
    <div className="page-fill active-alerts-page">
      <PageHeader
        title={t('nav:alerts.active')}
        writePermission="ack_alerts"
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.active') }]}
      />
      {/* "Clear all" clears the scope and the silenced chip with the columns, in ONE write: neither
          is a column, but both narrow this list, and a "clear all" that left them set would lie. */}
      <ListToolbar
        list={serverToolbarFilters(
          filterCols,
          { filters, setFilters },
          {
            active: scopeActive || onlySilenced,
            clear: (p) => {
              writeScope({ nodeId: '', groupId: '' })(p);
              p.delete('silenced');
            },
            onClear: () => setScope(allScope(t)),
          },
          counts,
        )}
        labels={columnLabels(columns)}
        count={{
          shown: shown.length,
          total: all.length,
          noun: (n) => t('common:noun.alert', { count: n }),
        }}
        leading={
          <ScopePicker
            value={{ ...scope, label: scopeLabel }}
            onChange={onScope}
            className="table-filter"
          />
        }
      />
      <WatchChips
        chips={watchChips({ silenced, maintenance: summary?.states.maintenance ?? 0, watch })}
        onlySilenced={onlySilenced}
        onToggleSilenced={toggleSilenced}
      />
      <DataTable
        tableId="alerts.active"
        rows={sorting.rows}
        columns={sorting.columns}
        sort={sorting.sort}
        onSortChange={sorting.setSort}
        filters={filters}
        onFiltersChange={setFilters}
        filterCounts={counts}
        rowKey={alertRowKey}
        // The Notes column stacks one note per line (ADR-243 decision 5), so a row is as tall as
        // its notes; a row with none keeps the standard height.
        autoRowHeight
        empty={
          all.length === 0 && noNodes ? (
            <EmptyState
              text={t('active.noNodes')}
              action={
                canConfig && (
                  <Button variant="primary" onClick={() => navigate(addNodeHref())}>
                    {t('nodes:add.node')}
                  </Button>
                )
              }
            />
          ) : all.length > 0 && filtered ? (
            t('active.emptyFiltered')
          ) : (
            t('active.empty')
          )
        }
      />
      {explaining && (
        <RcaModal
          node={explaining.node}
          check={explaining.check}
          onClose={() => setExplaining(null)}
        />
      )}
    </div>
  );
}

