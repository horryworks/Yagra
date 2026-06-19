// Reports (Dashboard ▸ Reports). Three tabs: Saved reports (generated runs, live over SSE),
// Templates (reusable report definitions), and Schedules (preset cadences). Reports are a shared
// resource — everyone views, only admins create/edit/run/delete (the write controls are hidden for
// non-admins; the server enforces it too).

import { useEffect, useState } from 'react';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { Badge } from '../components/ui/Badge';
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
} from '../types/api';
import { useReportRunsStore } from './store';
import { cadenceLabel } from './types';
import { ReportBuilder } from './ReportBuilder';
import { ReportViewer } from './ReportViewer';
import { ScheduleModal } from './ScheduleModal';
import './reports.css';

type Tab = 'saved' | 'templates' | 'schedules';

/** Status chip for a run (live progress while generating). */
function RunStatus({ run }: { run: ReportRun }) {
  switch (run.state) {
    case 'running':
      return <Badge tone="info">Generating {run.pct}%</Badge>;
    case 'queued':
      return <Badge tone="info">Queued</Badge>;
    case 'succeeded':
      return <Badge tone="up">Ready</Badge>;
    default:
      return <Badge tone="critical">Failed</Badge>;
  }
}

export function ReportsPage() {
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

  async function deleteDefinition(def: ReportDefinition) {
    if (!window.confirm(`Delete report template "${def.name}"? Saved runs are kept.`)) return;
    await api.deleteReportDefinition(def.id).catch(() => undefined);
    reloadDefs();
  }

  async function deleteRun(run: ReportRun) {
    if (!window.confirm(`Delete saved report "${run.name}"?`)) return;
    await api.deleteReportRun(run.id).catch(() => undefined);
    removeRun(run.id);
  }

  async function deleteSchedule(s: ReportSchedule) {
    if (!window.confirm(`Delete the schedule for "${s.definition_name}"?`)) return;
    await api.deleteReportSchedule(s.id).catch(() => undefined);
    reloadScheds();
  }

  // ── Column sets ──
  const runColumns: Column<ReportRun>[] = [
    { key: 'name', header: 'Report', width: '1.6fr', render: (r) => r.name },
    { key: 'status', header: 'Status', width: '160px', render: (r) => <RunStatus run={r} /> },
    {
      key: 'trigger',
      header: 'Trigger',
      width: '110px',
      render: (r) => <span className="muted">{r.trigger}</span>,
    },
    {
      key: 'when',
      header: 'Generated',
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
            View
          </Button>
          {isAdmin && (
            <Button variant="ghost" onClick={() => deleteRun(r)}>
              Delete
            </Button>
          )}
        </div>
      ),
    },
  ];

  const defColumns: Column<ReportDefinition>[] = [
    { key: 'name', header: 'Template', width: '1.6fr', render: (d) => d.name },
    {
      key: 'sections',
      header: 'Sections',
      width: '110px',
      render: (d) => <span className="muted">{d.spec?.sections?.length ?? 0}</span>,
    },
    {
      key: 'updated',
      header: 'Updated',
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
              {busy === d.id ? 'Running…' : 'Run now'}
            </Button>
          )}
          {isAdmin && (
            <Button variant="ghost" onClick={() => setBuilderFor(d)}>
              Edit
            </Button>
          )}
          {isAdmin && (
            <Button variant="ghost" onClick={() => deleteDefinition(d)}>
              Delete
            </Button>
          )}
        </div>
      ),
    },
  ];

  const schedColumns: Column<ReportSchedule>[] = [
    { key: 'name', header: 'Report', width: '1.4fr', render: (s) => s.definition_name },
    { key: 'cadence', header: 'Cadence', width: '1.4fr', render: (s) => cadenceLabel(s) },
    {
      key: 'next',
      header: 'Next run',
      width: '1fr',
      render: (s) => <span className="muted">{formatTimestamp(s.next_run_ms)}</span>,
    },
    {
      key: 'enabled',
      header: 'State',
      width: '110px',
      render: (s) =>
        s.enabled ? <Badge tone="up">Enabled</Badge> : <Badge tone="neutral">Paused</Badge>,
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
              Edit
            </Button>
            <Button variant="ghost" onClick={() => deleteSchedule(s)}>
              Delete
            </Button>
          </div>
        ) : (
          <span className="muted">—</span>
        ),
    },
  ];

  const tabs: { key: Tab; label: string; count: number }[] = [
    { key: 'saved', label: 'Saved reports', count: runs.length },
    { key: 'templates', label: 'Templates', count: definitions.length },
    { key: 'schedules', label: 'Schedules', count: schedules.length },
  ];

  return (
    <div className="page-fill">
      <PageHeader
        title="Reports"
        trail={[{ label: 'Dashboard' }, { label: 'Reports' }]}
        note="Build customizable reports, run them on a schedule, and save the results."
      />

      <div className="rp-tabs" role="tablist">
        {tabs.map((t) => (
          <button
            key={t.key}
            role="tab"
            aria-selected={tab === t.key}
            className={tab === t.key ? 'rp-tab active' : 'rp-tab'}
            onClick={() => setTab(t.key)}
          >
            {t.label}
            <span className="rp-tab-count">{t.count}</span>
          </button>
        ))}
      </div>

      {tab === 'saved' && (
        <>
          <TableToolbar>
            <TableSpacer />
            <ResultCount shown={runs.length} noun="saved reports" />
          </TableToolbar>
          <DataTable
            rows={runs}
            columns={runColumns}
            rowKey={(r) => r.id}
            onRowClick={(r) => setViewerRunId(r.id)}
            empty="No reports generated yet. Run a template to create one."
            loading={!runsLoaded}
          />
        </>
      )}

      {tab === 'templates' && (
        <>
          <TableToolbar>
            <TableSpacer />
            <ResultCount shown={definitions.length} noun="templates" />
            {isAdmin && (
              <Button variant="primary" onClick={() => setBuilderFor('new')}>
                New report
              </Button>
            )}
          </TableToolbar>
          <DataTable
            rows={definitions}
            columns={defColumns}
            rowKey={(d) => d.id}
            empty={
              isAdmin
                ? 'No report templates yet. Click "New report" to build one.'
                : 'No report templates yet.'
            }
            loading={defs.loading && definitions.length === 0}
          />
        </>
      )}

      {tab === 'schedules' && (
        <>
          <TableToolbar>
            <TableSpacer />
            <ResultCount shown={schedules.length} noun="schedules" />
            {isAdmin && (
              <Button
                variant="primary"
                disabled={definitions.length === 0}
                onClick={() => setScheduleFor('new')}
              >
                New schedule
              </Button>
            )}
          </TableToolbar>
          <DataTable
            rows={schedules}
            columns={schedColumns}
            rowKey={(s) => s.id}
            empty={
              definitions.length === 0
                ? 'Create a report template first, then schedule it.'
                : 'No schedules yet.'
            }
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
    </div>
  );
}
