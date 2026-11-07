// SPDX-License-Identifier: AGPL-3.0-only
// Analysis runs - the async-jobs list (ADR-022), backed by the real jobs API + SSE, drawn as a
// `DataTable` like every other list (ADR-246 Inc.7, chosen by the user). One row per job; the
// state column and the result column say where it is in its lifecycle:
//  - running   - spinner + %, and a live progress bar + phase as the result; Cancel
//  - done      - green dot + the result summary; View (its report)
//  - failed    - red dot + the reason; Retry (re-runs with the same config)
//  - cancelled - grey dot; Retry
// Progress and terminal states arrive over SSE (store.upsertJob); no client-side faking.

import { useNavigate } from 'react-router-dom';
import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { ListToolbar } from '../components/ui/ListToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { Button } from '../components/ui/Button';
import { columnLabels } from '../lib/listToolbar';
import { useClientFilters } from '../lib/useClientFilters';
import { runColumns } from './runFilters';
import { useTroubleshootStore } from './store';
import { useCan } from '../store';
import { seedAnalysisJobs } from './useTroubleshootStream';
import { reportPathFor, toolById } from './data';
import { inputFromJob } from './format';
import { relativeTimeMs } from '../lib/format';
import type { AnalysisJob } from '../types/api';

/** The state cell: a dot in the state's colour and its word, or a spinner and the % while it runs. */
function RunState({ job }: { job: AnalysisJob }) {
  const { t } = useTranslation('troubleshoot');
  if (job.state === 'running') {
    return (
      <span className="ts-run-state">
        <span className="ts-run-spin" />
        {Math.round(job.pct)}%
      </span>
    );
  }
  const color =
    job.state === 'done'
      ? 'var(--status-up)'
      : job.state === 'failed'
        ? 'var(--status-critical)'
        : 'var(--text-tertiary)';
  return (
    <span className="ts-run-state">
      <span className="ts-run-dot" style={{ background: color }} />
      {t(`runs.state.${job.state === 'unknown' ? 'cancelled' : job.state}`)}
    </span>
  );
}

/** The result cell: the live progress while it runs, the summary once done, the reason it failed. */
function RunResult({ job }: { job: AnalysisJob }) {
  const { t } = useTranslation('troubleshoot');
  if (job.state === 'running') {
    const pct = Math.round(job.pct);
    return (
      <span className="ts-run-prog">
        <span className="ts-run-bar">
          <span className="ts-run-bar-fill" style={{ width: `${pct}%` }} />
        </span>
        <span className="ts-run-phase">{job.phase ?? t('runs.running')}</span>
      </span>
    );
  }
  if (job.state === 'done') {
    return <>{job.summary ?? t('runs.findingCount', { count: job.finding_count })}</>;
  }
  if (job.state === 'failed') {
    return <span className="ts-run-failed">{job.error ?? t('runs.analysisError')}</span>;
  }
  return <>{'—'}</>;
}

/** The one action a run offers: Cancel while it runs, View once done, Retry after it stops. */
function RunAction({ job }: { job: AnalysisJob }) {
  const { t } = useTranslation('troubleshoot');
  // `POST /analysis/jobs` and `…/cancel` take `RequireAckAlerts`; reading runs stays View. A control
  // the caller may not use is not drawn (ADR-056) — a Viewer used to see every Run button here and
  // get "could not start" with no reason.
  const canRun = useCan('ack_alerts');
  const navigate = useNavigate();
  const cancelJob = useTroubleshootStore((s) => s.cancelJob);
  const createJob = useTroubleshootStore((s) => s.createJob);
  const showToast = useTroubleshootStore((s) => s.showToast);

  // `job.tool` is a bare string on the wire, so the catalog lookup is also what narrows it.
  const tool = toolById(job.tool);
  // `tool.name` is an i18next key (see data.ts); an unknown tool falls back to its raw backend id.
  const name = tool ? t(tool.name) : job.tool;
  // Every tool in the catalog has a report, so the path comes from the job's own tool key. A job
  // from a newer core naming a tool this build doesn't have has no report to open — send it to the
  // catalog, which is where `/troubleshoot/report/<unknown>` redirects anyway.
  const reportPath = tool ? reportPathFor(tool.id) : '/troubleshoot';

  const view = () => navigate(`${reportPath}?job=${encodeURIComponent(job.id)}`);

  const retry = async () => {
    try {
      const fresh = await createJob(inputFromJob(job));
      showToast(t('runs.toast.requeued', { name }), `${reportPath}?job=${fresh.id}`);
    } catch {
      showToast(t('runs.toast.requeueFailed'));
    }
  };

  if (job.state === 'running') {
    return canRun ? (
      <button className="ts-linkbtn ts-run-link" onClick={() => void cancelJob(job.id)}>
        {t('common:actions.cancel')}
      </button>
    ) : null;
  }
  if (job.state === 'done') {
    return (
      <button className="ts-linkbtn ts-run-link" onClick={view}>
        {t('actions.view')} {'→'}
      </button>
    );
  }
  return canRun ? (
    <button className="ts-linkbtn ts-run-link" onClick={() => void retry()}>
      {t('common:actions.retry')}
    </button>
  ) : null;
}

export function AnalysisRuns() {
  const { t } = useTranslation('troubleshoot');
  const jobs = useTroubleshootStore((s) => s.jobs);
  const loaded = useTroubleshootStore((s) => s.loaded);
  const loadFailed = useTroubleshootStore((s) => s.loadFailed);
  // Each filter sits on the column it narrows, so the column key is the URL key (`state`, `tool`,
  // `q`), unchanged from the toolbar funnel these filters opened from before the list was a table.
  const filterCols = useMemo(() => runColumns(t), [t]);
  const columns = useMemo(() => {
    const cols: Column<AnalysisJob>[] = [
      {
        key: 'state',
        header: t('runs.cols.state'),
        width: '120px',
        render: (j) => <RunState job={j} />,
      },
      {
        key: 'tool',
        header: t('runs.cols.tool'),
        width: '200px',
        // `job.tool` is a bare string on the wire; an unknown tool falls back to its backend id.
        render: (j) => {
          const tool = toolById(j.tool);
          return <span className="ts-run-tool">{tool ? t(tool.name) : j.tool}</span>;
        },
      },
      {
        key: 'q',
        header: t('runs.cols.scope'),
        width: '1fr',
        render: (j) => j.scope_label,
      },
      {
        key: 'result',
        header: t('runs.cols.result'),
        width: '1.5fr',
        render: (j) => <RunResult job={j} />,
      },
      {
        key: 'time',
        header: t('runs.cols.time'),
        width: '100px',
        // A running job's age is how long ago it started; a stopped one's, when it stopped.
        render: (j) => relativeTimeMs(j.state === 'running' ? j.created_ms : j.finished_ms),
      },
      {
        key: 'actions',
        header: t('runs.cols.actions'),
        width: '100px',
        align: 'right',
        render: (j) => <RunAction job={j} />,
      },
    ];
    for (const c of cols) c.filter = filterCols.find((f) => f.key === c.key)?.filter;
    return cols;
  }, [t, filterCols]);
  const filtering = useClientFilters(columns, jobs);
  const { filters, setFilters, shown, counts, anyFiltered } = filtering;

  return (
    <>
      <ListToolbar
        list={filtering}
        labels={columnLabels(columns)}
        count={{
          shown: shown.length,
          total: jobs.length,
          noun: (n) => t('runs.noun', { count: n }),
        }}
      />
      {loadFailed && jobs.length === 0 && (
        <p className="form-error" role="alert">
          {t('runs.loadFailed')}{' '}
          <Button onClick={seedAnalysisJobs}>{t('common:actions.retry')}</Button>
        </p>
      )}
      <DataTable
        tableId="troubleshoot.runs"
        rows={shown}
        columns={columns}
        rowKey={(j) => j.id}
        filters={filters}
        onFiltersChange={setFilters}
        filterCounts={counts}
        loading={!loaded && !loadFailed}
        empty={anyFiltered ? t('common:filter.noMatch') : t('runs.empty')}
      />
    </>
  );
}
