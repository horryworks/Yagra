// SPDX-License-Identifier: AGPL-3.0-only
// Scheduled analyses (`/troubleshoot/scheduled`) — recurring Troubleshoot runs.
//
// The runs list says what has happened; this says what will. Data-table standard v2: a toolbar with
// the add action over the shared `DataTable`. The list is bounded by what an operator typed, so it
// is not paged.
//
// `last_status` is rendered from a `Record` keyed by the union, never a switch with a `default:` —
// that is the bug `reports/runStatus.ts` was written to fix, and `busy` is exactly the variant it
// would misreport: a deferred fire is not a failure, it is a fire that will happen a minute later.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api, errMsg } from '../services/api';
import { useAuthStore } from '../store';
import type { AnalysisSchedule, AnalysisScheduleStatus } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Badge, type Tone } from '../components/ui/Badge';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { useClientFilters } from '../lib/useClientFilters';
import { scheduleFilters } from './scheduleFilters';
import { TimeCell } from '../components/ui/tableCells';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { EditIcon, TrashIcon } from '../components/ui/icons';
import { cadenceLabel } from '../lib/cadence';
import { toolById } from './data';
import { ScheduleModal } from './ScheduleModal';
import './troubleshoot.css';

/** Tone + label per firing outcome. A `Record`, so a new status is a compile error here rather than
 *  a silent mislabel — `busy` in particular must not read as a failure. */
const STATUS: Record<AnalysisScheduleStatus, { tone: Tone; labelKey: string }> = {
  queued: { tone: 'up', labelKey: 'schedule.status.queued' },
  busy: { tone: 'info', labelKey: 'schedule.status.busy' },
  error: { tone: 'critical', labelKey: 'schedule.status.error' },
  unknown: { tone: 'neutral', labelKey: 'schedule.status.unknown' },
};

function scheduleColumns(
  t: TFunction,
  onEdit: (s: AnalysisSchedule) => void,
  onDelete: (s: AnalysisSchedule) => void,
): Column<AnalysisSchedule>[] {
  const specs = scheduleFilters(t);
  const cols: Column<AnalysisSchedule>[] = [
    {
      key: 'tool',
      header: t('schedule.cols.analysis'),
      width: '1fr',
      // A bare string on the wire, so the catalog lookup is also what narrows it.
      render: (s) => {
        const tool = toolById(s.tool);
        return <span>{tool ? t(tool.name) : s.tool}</span>;
      },
    },
    {
      key: 'scope',
      header: t('schedule.cols.scope'),
      width: '1fr',
      render: (s) => <span className="muted">{s.scope_label}</span>,
    },
    {
      key: 'cadence',
      header: t('schedule.cols.cadence'),
      width: '1.2fr',
      render: (s) => <span>{cadenceLabel(t, s)}</span>,
    },
    {
      key: 'next',
      header: t('schedule.cols.next'),
      width: '190px',
      // A disabled schedule still carries a computed next_run_at; showing it would claim a run
      // that will not happen.
      render: (s) =>
        s.enabled ? (
          <TimeCell iso={new Date(s.next_run_ms).toISOString()} />
        ) : (
          <span className="muted">{t('schedule.paused')}</span>
        ),
    },
    {
      key: 'last',
      header: t('schedule.cols.last'),
      width: '150px',
      render: (s) =>
        s.last_status ? (
          <Badge tone={STATUS[s.last_status].tone}>{t(STATUS[s.last_status].labelKey)}</Badge>
        ) : (
          <span className="muted">—</span>
        ),
    },
    {
      key: 'actions',
      header: '',
      width: '56px',
      align: 'right',
      render: (s) => (
        <OverflowMenu
          actions={[
            {
              label: t('common:actions.edit'),
              icon: <EditIcon width={15} height={15} />,
              onClick: () => onEdit(s),
            },
            {
              label: t('common:actions.delete'),
              icon: <TrashIcon width={15} height={15} />,
              onClick: () => onDelete(s),
              danger: true,
            },
          ]}
        />
      ),
    },
  ];
  for (const c of cols) c.filter = specs[c.key];
  return cols;
}

export function ScheduledPage() {
  const { t } = useTranslation('troubleshoot');
  const authed = useAuthStore((s) => s.authed);
  const role = useAuthStore((s) => s.role);
  const [rows, setRows] = useState<AnalysisSchedule[]>([]);
  const [flowEnabled, setFlowEnabled] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<AnalysisSchedule | null>(null);
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<AnalysisSchedule | null>(null);
  const [sheet, setSheet] = useState(false);

  // Creating or editing a schedule is `AckAlerts` at the edge, like launching a run — so the
  // action is offered only to a role that holds it, rather than failing on save.
  const canWrite = role === 'operator' || role === 'admin';

  const load = useCallback(() => {
    setError(null);
    api
      .listAnalysisSchedules()
      .then(setRows)
      .catch((e: unknown) => setError(errMsg(e, t('schedule.err.load'))))
      .finally(() => setLoading(false));
  }, [t]);

  useEffect(() => {
    if (!authed) return;
    load();
    // The flow tier decides which analyses may be scheduled at all; the same call the app already
    // makes for the login gate carries it.
    api
      .getConfig()
      .then((c) => setFlowEnabled(c.flow_enabled === true))
      .catch(() => undefined);
  }, [authed, load]);

  const columns = useMemo(() => scheduleColumns(t, setEditing, setDeleting), [t]);
  // Client-side: the list is bounded by what an operator set up, not by fleet size
  // (ui-conventions). URL-backed — one table on this route.
  const { filterCols, filters, setFilters, clear, shown, counts, anyFiltered } = useClientFilters(
    columns,
    rows,
    { url: true },
  );

  const saved = () => {
    setCreating(false);
    setEditing(null);
    load();
  };

  return (
    <div className="page-fill">
      <PageHeader
        title={t('nav:troubleshoot.scheduled')}
        trail={[
          { label: t('nav:sections.troubleshoot'), to: '/troubleshoot' },
          { label: t('nav:troubleshoot.scheduled') },
        ]}
        note={t('schedule.pageNote')}
      />

      {!authed ? (
        <Card>
          <p className="muted">{t('schedule.signInPrompt')}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <FilterButton
              columns={filterCols}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters columns={filterCols} filters={filters} onClear={clear} />
            <TableSpacer />
            <ResultCount
              shown={shown.length}
              total={anyFiltered ? rows.length : undefined}
              noun={t('schedule.schedule', { count: shown.length })}
            />
            {canWrite && (
              <Button variant="primary" onClick={() => setCreating(true)}>
                {t('schedule.add')}
              </Button>
            )}
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <DataTable
            rows={shown}
            columns={columns}
            filters={filters}
            onFiltersChange={setFilters}
            filterCounts={counts}
            rowKey={(s) => s.id}
            loading={loading}
            empty={anyFiltered ? t('common:filter.noMatch') : t('schedule.empty')}
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={filters}
              onChange={setFilters}
              counts={counts}
              labels={{
                tool: t('schedule.cols.analysis'),
                scope: t('schedule.cols.scope'),
                next: t('schedule.cols.next'),
                last: t('schedule.cols.last'),
              }}
              onClose={() => setSheet(false)}
            />
          )}
        </>
      )}

      {(creating || editing) && (
        <ScheduleModal
          schedule={editing}
          flowEnabled={flowEnabled}
          onClose={() => {
            setCreating(false);
            setEditing(null);
          }}
          onSaved={saved}
        />
      )}

      {deleting && (
        <ConfirmDeleteModal
          title={t('schedule.deleteTitle')}
          errorFallback={t('schedule.err.deleteFailed')}
          onConfirm={() => api.deleteAnalysisSchedule(deleting.id)}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            load();
          }}
        >
          {t('schedule.deleteBody', {
            name: toolById(deleting.tool) ? t(toolById(deleting.tool)!.name) : deleting.tool,
          })}
        </ConfirmDeleteModal>
      )}
    </div>
  );
}
