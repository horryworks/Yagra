// SPDX-License-Identifier: AGPL-3.0-only
// Reports (Dashboard ▸ Reports). Three tabs: Saved reports (generated runs, live over SSE),
// Templates (reusable report definitions), and Schedules (preset cadences). Reports are a shared
// resource — everyone views, only admins create/edit/run/delete (the write controls are hidden for
// non-admins; the server enforces it too).

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { Badge } from '../components/ui/Badge';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { ClearFilters } from '../components/ui/ClearFilters';
import { MobileFilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { useClientFilters } from '../lib/useClientFilters';
import {
  definitionFilters,
  reportScheduleFilters,
  savedRunFilters,
} from './reportListFilters';
import { usePolled } from '../dashboard/usePolled';
import { api } from '../services/api';
import { subscribeReportRuns } from '../services/sse';
import { useAuthStore } from '../store';
import { formatTimestamp } from '../lib/format';
import type {
  ReportDefinition,
  ReportRun,
  ReportSchedule,
  ReportSectionDef,
  ReportSpec,
} from '../types/api';
import { useReportRunsStore } from './store';
import { cadenceLabel } from '../lib/cadence';
import { RUN_STATUS } from './runStatus';
import { ReportBuilder } from './ReportBuilder';
import { ReportViewer } from './ReportViewer';
import { ScheduleModal } from './ScheduleModal';
import './reports.css';

type Tab = 'saved' | 'templates' | 'schedules';

/** Status chip for a run (live progress while generating). Read from the registry rather than a
 *  switch, so a state added to the backend cannot fall through to "failed" — see `runStatus.ts`. */
function RunStatus({ run }: { run: ReportRun }) {
  const { t } = useTranslation('reports');
  const spec = RUN_STATUS[run.state];
  return (
    <Badge tone={spec.tone}>
      {spec.showsPct ? t(spec.labelKey, { pct: run.pct }) : t(spec.labelKey)}
    </Badge>
  );
}

export function ReportsPage() {
  const { t } = useTranslation('reports');
  const role = useAuthStore((s) => s.role);
  const isAdmin = role === 'admin';
  const [tab, setTab] = useState<Tab>('saved');

  // Saved runs: seed from the API, keep live over SSE.
  const runs = useReportRunsStore((s) => s.runs);
  const runsLoaded = useReportRunsStore((s) => s.loaded);
  const setRuns = useReportRunsStore((s) => s.setRuns);
  const upsertRun = useReportRunsStore((s) => s.upsertRun);
  const removeRun = useReportRunsStore((s) => s.removeRun);
  useEffect(() => {
    let alive = true;
    api
      .listReportRuns(100)
      .then((r) => {
        if (alive) setRuns(r);
      })
      .catch(() => undefined);
    const unsub = subscribeReportRuns((run) => upsertRun(run));
    return () => {
      alive = false;
      unsub();
    };
  }, [setRuns, upsertRun]);

  // Catalog + definitions + schedules. Manual reload bumps refetch immediately after a mutation.
  const sections = usePolled(() => api.listReportSections(), [], 300_000);
  const [defsReload, setDefsReload] = useState(0);
  const defs = usePolled(() => api.listReportDefinitions(), [defsReload], 30_000);
  const [schedReload, setSchedReload] = useState(0);
  const scheds = usePolled(() => api.listReportSchedules(), [schedReload], 30_000);

  const reloadDefs = () => setDefsReload((n) => n + 1);
  const reloadScheds = () => setSchedReload((n) => n + 1);

  // Modal targets: a definition (or 'new') for the builder; a run id for the viewer; a schedule
  // (or 'new') for the schedule editor.
  const [builderFor, setBuilderFor] = useState<ReportDefinition | 'new' | null>(null);
  const [viewerRunId, setViewerRunId] = useState<string | null>(null);
  const [scheduleFor, setScheduleFor] = useState<ReportSchedule | 'new' | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  // Destructive-action consent via the shared ConfirmDeleteModal — a failed delete keeps the
  // dialog open and shows the message rather than closing silently.
  const [confirm, setConfirm] = useState<{ text: string; run: () => Promise<void> } | null>(null);
  // All three tabs filter in the browser. Templates and schedules because they are bounded by what
  // an admin authored rather than by fleet size (ui-conventions); saved runs because the store
  // behind it is SSE-fed and would undo a filtered fetch on the next progress frame — the reason is
  // written out in `reportListFilters.ts`, beside the predicate it explains.
  // ⚠️ And **not** URL-backed, unlike every other Inc.3 screen: three tables share this route and
  // the column key is the URL key, so `name` would be written by two of them at once.
  const [sheet, setSheet] = useState<'saved' | 'templates' | 'schedules' | null>(null);

  const catalog: ReportSectionDef[] = sections.data ?? [];
  const definitions: ReportDefinition[] = defs.data ?? [];
  const schedules: ReportSchedule[] = scheds.data ?? [];

  async function runNow(def: ReportDefinition) {
    setBusy(def.id);
    try {
      const run = await api.runReport(def.id);
      upsertRun(run);
      setTab('saved');
    } catch {
      // surfaced by the next list refresh; keep the UI responsive
    } finally {
      setBusy(null);
    }
  }

  function deleteDefinition(def: ReportDefinition) {
    setConfirm({
      text: t('confirm.deleteTemplate', { name: def.name }),
      run: async () => {
        await api.deleteReportDefinition(def.id);
        reloadDefs();
      },
    });
  }

  function deleteRun(run: ReportRun) {
    setConfirm({
      text: t('confirm.deleteRun', { name: run.name }),
      run: async () => {
        await api.deleteReportRun(run.id);
        removeRun(run.id);
      },
    });
  }

  function deleteSchedule(s: ReportSchedule) {
    setConfirm({
      text: t('confirm.deleteSchedule', { name: s.definition_name }),
      run: async () => {
        await api.deleteReportSchedule(s.id);
        reloadScheds();
      },
    });
  }

  // ── Column sets ──
  const runSpecs = savedRunFilters(t);
  const runColumns: Column<ReportRun>[] = [
    { key: 'name', header: t('runs.cols.report'), width: '1.6fr', render: (r) => r.name },
    { key: 'status', header: t('runs.cols.status'), width: '160px', render: (r) => <RunStatus run={r} /> },
    {
      key: 'trigger',
      header: t('runs.cols.trigger'),
      width: '110px',
      render: (r) => <span className="muted">{t(`trigger.${r.trigger}`)}</span>,
    },
    {
      key: 'when',
      header: t('runs.cols.generated'),
      width: '1fr',
      render: (r) => <span className="muted">{formatTimestamp(r.created_ms)}</span>,
    },
    {
      key: 'actions',
      header: '',
      width: '160px',
      align: 'right',
      render: (r) => (
        <div className="rp-actions" onClick={(e) => e.stopPropagation()}>
          <Button variant="ghost" onClick={() => setViewerRunId(r.id)}>
            {t('runs.view')}
          </Button>
          {isAdmin && (
            <Button variant="ghost" onClick={() => deleteRun(r)}>
              {t('common:actions.delete')}
            </Button>
          )}
        </div>
      ),
    },
  ];
  for (const c of runColumns) c.filter = runSpecs[c.key];

  const defSpecs = definitionFilters(t);
  const defColumns: Column<ReportDefinition>[] = [
    { key: 'name', header: t('defs.cols.template'), width: '1.6fr', render: (d) => d.name },
    {
      key: 'sections',
      header: t('defs.cols.sections'),
      width: '110px',
      // `spec` is opaque JSON to the backend, hence `unknown` on the wire; this is the WebUI's
      // reading of the document it owns.
      render: (d) => (
        <span className="muted">{(d.spec as ReportSpec | null)?.sections?.length ?? 0}</span>
      ),
    },
    {
      key: 'updated',
      header: t('defs.cols.updated'),
      width: '1fr',
      render: (d) => <span className="muted">{formatTimestamp(d.updated_ms)}</span>,
    },
    {
      key: 'actions',
      header: '',
      width: '260px',
      align: 'right',
      render: (d) => (
        <div className="rp-actions">
          {isAdmin && (
            <Button variant="primary" disabled={busy === d.id} onClick={() => runNow(d)}>
              {busy === d.id ? t('defs.running') : t('defs.runNow')}
            </Button>
          )}
          {isAdmin && (
            <Button variant="ghost" onClick={() => setBuilderFor(d)}>
              {t('common:actions.edit')}
            </Button>
          )}
          {isAdmin && (
            <Button variant="ghost" onClick={() => deleteDefinition(d)}>
              {t('common:actions.delete')}
            </Button>
          )}
        </div>
      ),
    },
  ];
  for (const c of defColumns) c.filter = defSpecs[c.key];

  const schedSpecs = reportScheduleFilters(t);
  const schedColumns: Column<ReportSchedule>[] = [
    { key: 'name', header: t('scheds.cols.report'), width: '1.4fr', render: (s) => s.definition_name },
    {
      key: 'cadence',
      header: t('scheds.cols.cadence'),
      width: '1.4fr',
      render: (s) => cadenceLabel(t, s),
    },
    {
      key: 'next',
      header: t('scheds.cols.nextRun'),
      width: '1fr',
      render: (s) => <span className="muted">{formatTimestamp(s.next_run_ms)}</span>,
    },
    {
      key: 'enabled',
      header: t('scheds.cols.state'),
      width: '110px',
      render: (s) =>
        s.enabled ? (
          <Badge tone="up">{t('scheds.enabled')}</Badge>
        ) : (
          <Badge tone="neutral">{t('scheds.paused')}</Badge>
        ),
    },
    {
      key: 'actions',
      header: '',
      width: '170px',
      align: 'right',
      render: (s) =>
        isAdmin ? (
          <div className="rp-actions">
            <Button variant="ghost" onClick={() => setScheduleFor(s)}>
              {t('common:actions.edit')}
            </Button>
            <Button variant="ghost" onClick={() => deleteSchedule(s)}>
              {t('common:actions.delete')}
            </Button>
          </div>
        ) : (
          <span className="muted">—</span>
        ),
    },
  ];
  for (const c of schedColumns) c.filter = schedSpecs[c.key];

  const runF = useClientFilters(runColumns, runs);
  const defF = useClientFilters(defColumns, definitions);
  const schedF = useClientFilters(schedColumns, schedules);

  const tabs: { key: Tab; label: string; count: number }[] = [
    { key: 'saved', label: t('tabs.saved'), count: runs.length },
    { key: 'templates', label: t('tabs.templates'), count: definitions.length },
    { key: 'schedules', label: t('tabs.schedules'), count: schedules.length },
  ];

  return (
    <div className="page-fill">
      <PageHeader
        title={t('nav:dashboard.reports')}
        trail={[{ label: t('nav:sections.dashboard') }, { label: t('nav:dashboard.reports') }]}
        note={t('page.note')}
      />

      <div className="rp-tabs" role="tablist">
        {tabs.map((tb) => (
          <button
            key={tb.key}
            role="tab"
            aria-selected={tab === tb.key}
            className={tab === tb.key ? 'rp-tab active' : 'rp-tab'}
            onClick={() => setTab(tb.key)}
          >
            {tb.label}
            <span className="rp-tab-count">{tb.count}</span>
          </button>
        ))}
      </div>

      {tab === 'saved' && (
        <>
          <TableToolbar>
            <MobileFilterButton
              columns={runF.filterCols}
              filters={runF.filters}
              onOpen={() => setSheet('saved')}
            />
            <ClearFilters
              columns={runF.filterCols}
              filters={runF.filters}
              onClear={runF.clear}
            />
            <TableSpacer />
            <ResultCount
              shown={runF.shown.length}
              total={runF.anyFiltered ? runs.length : undefined}
              noun={t('noun.savedReport', { count: runF.shown.length })}
            />
          </TableToolbar>
          <DataTable
            rows={runF.shown}
            columns={runColumns}
            filters={runF.filters}
            onFiltersChange={runF.setFilters}
            filterCounts={runF.counts}
            rowKey={(r) => r.id}
            onRowClick={(r) => setViewerRunId(r.id)}
            empty={runF.anyFiltered ? t('common:filter.noMatch') : t('runs.empty')}
            loading={!runsLoaded}
          />
          {sheet === 'saved' && (
            <MobileFilterSheet
              columns={runF.filterCols}
              filters={runF.filters}
              onChange={runF.setFilters}
              counts={runF.counts}
              labels={{
                name: t('runs.cols.report'),
                status: t('runs.cols.status'),
                trigger: t('runs.cols.trigger'),
                when: t('runs.cols.generated'),
              }}
              onClose={() => setSheet(null)}
            />
          )}
        </>
      )}

      {tab === 'templates' && (
        <>
          <TableToolbar>
            <MobileFilterButton
              columns={defF.filterCols}
              filters={defF.filters}
              onOpen={() => setSheet('templates')}
            />
            <ClearFilters
              columns={defF.filterCols}
              filters={defF.filters}
              onClear={defF.clear}
            />
            <TableSpacer />
            <ResultCount
              shown={defF.shown.length}
              total={defF.anyFiltered ? definitions.length : undefined}
              noun={t('common:noun.template', { count: defF.shown.length })}
            />
            {isAdmin && (
              <Button variant="primary" onClick={() => setBuilderFor('new')}>
                {t('defs.newReport')}
              </Button>
            )}
          </TableToolbar>
          <DataTable
            rows={defF.shown}
            columns={defColumns}
            filters={defF.filters}
            onFiltersChange={defF.setFilters}
            filterCounts={defF.counts}
            rowKey={(d) => d.id}
            empty={
              defF.anyFiltered
                ? t('common:filter.noMatch')
                : isAdmin
                  ? t('defs.emptyAdmin')
                  : t('defs.empty')
            }
            loading={defs.loading && definitions.length === 0}
          />
          {sheet === 'templates' && (
            <MobileFilterSheet
              columns={defF.filterCols}
              filters={defF.filters}
              onChange={defF.setFilters}
              counts={defF.counts}
              labels={{
                name: t('defs.cols.template'),
                updated: t('defs.cols.updated'),
              }}
              onClose={() => setSheet(null)}
            />
          )}
        </>
      )}

      {tab === 'schedules' && (
        <>
          <TableToolbar>
            <MobileFilterButton
              columns={schedF.filterCols}
              filters={schedF.filters}
              onOpen={() => setSheet('schedules')}
            />
            <ClearFilters
              columns={schedF.filterCols}
              filters={schedF.filters}
              onClear={schedF.clear}
            />
            <TableSpacer />
            <ResultCount
              shown={schedF.shown.length}
              total={schedF.anyFiltered ? schedules.length : undefined}
              noun={t('noun.schedule', { count: schedF.shown.length })}
            />
            {isAdmin && (
              <Button
                variant="primary"
                disabled={definitions.length === 0}
                onClick={() => setScheduleFor('new')}
              >
                {t('scheds.newSchedule')}
              </Button>
            )}
          </TableToolbar>
          <DataTable
            rows={schedF.shown}
            columns={schedColumns}
            filters={schedF.filters}
            onFiltersChange={schedF.setFilters}
            filterCounts={schedF.counts}
            rowKey={(s) => s.id}
            empty={
              schedF.anyFiltered
                ? t('common:filter.noMatch')
                : definitions.length === 0
                  ? t('scheds.emptyNoDefs')
                  : t('scheds.empty')
            }
            loading={scheds.loading && schedules.length === 0}
          />
          {sheet === 'schedules' && (
            <MobileFilterSheet
              columns={schedF.filterCols}
              filters={schedF.filters}
              onChange={schedF.setFilters}
              counts={schedF.counts}
              labels={{
                name: t('scheds.cols.report'),
                next: t('scheds.cols.nextRun'),
                enabled: t('scheds.cols.state'),
              }}
              onClose={() => setSheet(null)}
            />
          )}
        </>
      )}

      {builderFor && (
        <ReportBuilder
          catalog={catalog}
          definition={builderFor === 'new' ? null : builderFor}
          onClose={() => setBuilderFor(null)}
          onSaved={() => {
            setBuilderFor(null);
            reloadDefs();
          }}
        />
      )}

      {viewerRunId && (
        <ReportViewer runId={viewerRunId} onClose={() => setViewerRunId(null)} />
      )}

      {scheduleFor && (
        <ScheduleModal
          definitions={definitions}
          schedule={scheduleFor === 'new' ? null : scheduleFor}
          onClose={() => setScheduleFor(null)}
          onSaved={() => {
            setScheduleFor(null);
            reloadScheds();
          }}
        />
      )}

      {confirm && (
        <ConfirmDeleteModal
          title={t('common:actions.delete')}
          onConfirm={confirm.run}
          errorFallback={t('err.delete')}
          onClose={() => setConfirm(null)}
          onDone={() => setConfirm(null)}
        >
          {confirm.text}
        </ConfirmDeleteModal>
      )}
    </div>
  );
}
