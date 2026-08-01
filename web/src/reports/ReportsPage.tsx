// SPDX-License-Identifier: AGPL-3.0-only
// Reports (Dashboard ▸ Reports). Three tabs: Saved reports (generated runs, live over SSE),
// Templates (reusable report definitions), and Schedules (preset cadences). Reports are a shared
// resource — everyone views, only admins create/edit/run/delete (the write controls are hidden for
// non-admins; the server enforces it too).

import { useEffect, useState } from 'react';
import type { TFunction } from 'i18next';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { Badge } from '../components/ui/Badge';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
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
import { cadenceLabel } from './types';
import { RUN_STATUS } from './runStatus';
import { ReportBuilder } from './ReportBuilder';
import { ReportViewer } from './ReportViewer';
import { ScheduleModal } from './ScheduleModal';
import './reports.css';

type Tab = 'saved' | 'templates' | 'schedules';

/** `cadenceLabel` reads both day fields; a stored schedule may omit the one its cadence doesn't
 *  use. The cadence itself is now typed, so there is nothing left to narrow. */
function scheduleCadence(t: TFunction, s: ReportSchedule): string {
  return cadenceLabel(t, {
    frequency: s.frequency,
    day_of_week: s.day_of_week ?? null,
    day_of_month: s.day_of_month ?? null,
    at_hour: s.at_hour,
    at_minute: s.at_minute,
  });
}

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

  const schedColumns: Column<ReportSchedule>[] = [
    { key: 'name', header: t('scheds.cols.report'), width: '1.4fr', render: (s) => s.definition_name },
    {
      key: 'cadence',
      header: t('scheds.cols.cadence'),
      width: '1.4fr',
      render: (s) => scheduleCadence(t, s),
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
            <TableSpacer />
            <ResultCount shown={runs.length} noun={t('noun.savedReport', { count: runs.length })} />
          </TableToolbar>
          <DataTable
            rows={runs}
            columns={runColumns}
            rowKey={(r) => r.id}
            onRowClick={(r) => setViewerRunId(r.id)}
            empty={t('runs.empty')}
            loading={!runsLoaded}
          />
        </>
      )}

      {tab === 'templates' && (
        <>
          <TableToolbar>
            <TableSpacer />
            <ResultCount
              shown={definitions.length}
              noun={t('common:noun.template', { count: definitions.length })}
            />
            {isAdmin && (
              <Button variant="primary" onClick={() => setBuilderFor('new')}>
                {t('defs.newReport')}
              </Button>
            )}
          </TableToolbar>
          <DataTable
            rows={definitions}
            columns={defColumns}
            rowKey={(d) => d.id}
            empty={isAdmin ? t('defs.emptyAdmin') : t('defs.empty')}
            loading={defs.loading && definitions.length === 0}
          />
        </>
      )}

      {tab === 'schedules' && (
        <>
          <TableToolbar>
            <TableSpacer />
            <ResultCount
              shown={schedules.length}
              noun={t('noun.schedule', { count: schedules.length })}
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
            rows={schedules}
            columns={schedColumns}
            rowKey={(s) => s.id}
            empty={definitions.length === 0 ? t('scheds.emptyNoDefs') : t('scheds.empty')}
            loading={scheds.loading && schedules.length === 0}
          />
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
