// SPDX-License-Identifier: AGPL-3.0-only
// Analysis runs — the async-jobs list (ADR-022), now backed by the real jobs API + SSE. One
// CSS-grid row per job in its lifecycle state:
//  • running   — spinner + live progress bar + phase + %; Cancel
//  • done      — green dot + result summary + relative time; View → (report, if one exists)
//  • failed    — red dot + reason; Retry (re-runs with the same config)
//  • cancelled — grey dot + "cancelled"; Retry
// Progress and terminal states arrive over SSE (store.upsertJob); no client-side faking.

import { useNavigate } from 'react-router-dom';
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ResultCount, TableSpacer, TableToolbar } from '../components/ui/TableToolbar';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterBar } from '../components/ui/FilterBar';
import { MobileFilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import {
  defaultFilters,
  isAnyFiltered,
  type FilterState,
} from '../lib/columnFilter';
import { facetCounts } from '../lib/filterCounts';
import { buildPredicate } from '../lib/filterPredicate';
import { runColumns, runFilterLabels } from './runFilters';
import { useTroubleshootStore } from './store';
import { reportPathFor, toolById } from './data';
import { relTime, inputFromJob } from './format';
import type { AnalysisJob } from '../types/api';

function RunRow({ job }: { job: AnalysisJob }) {
  const { t } = useTranslation('troubleshoot');
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

  const main = (
    <div className="ts-run-main">
      <span className="ts-run-tool">{name}</span>
      <span className="ts-run-scope">{job.scope_label}</span>
    </div>
  );

  if (job.state === 'running') {
    const pct = Math.round(job.pct);
    return (
      <div className="ts-run">
        <div className="ts-run-status">
          <div className="ts-run-spin" />
        </div>
        {main}
        <div className="ts-run-prog">
          <div className="ts-run-bar">
            <div className="ts-run-bar-fill" style={{ width: `${pct}%` }} />
          </div>
          <span className="ts-run-phase">{job.phase ?? t('runs.running')}</span>
        </div>
        <div className="ts-run-eta">{pct}%</div>
        <div className="ts-run-time">{relTime(job.created_ms)}</div>
        <div className="ts-run-action">
          <button className="ts-linkbtn ts-run-link" onClick={() => void cancelJob(job.id)}>
            {t('common:actions.cancel')}
          </button>
        </div>
      </div>
    );
  }

  if (job.state === 'done') {
    return (
      <div className="ts-run">
        <div className="ts-run-status">
          <span className="ts-run-dot" style={{ background: 'var(--status-up)' }} />
        </div>
        {main}
        <div className="ts-run-findings">
          {job.summary ?? t('runs.findingCount', { count: job.finding_count })}
        </div>
        <div className="ts-run-eta">{t('runs.done')}</div>
        <div className="ts-run-time">{relTime(job.finished_ms)}</div>
        <div className="ts-run-action">
          <button className="ts-linkbtn ts-run-link" onClick={view}>
            {t('actions.view')} →
          </button>
        </div>
      </div>
    );
  }

  // failed or cancelled
  const failed = job.state === 'failed';
  return (
    <div className="ts-run">
      <div className="ts-run-status">
        <span
          className="ts-run-dot"
          style={{ background: failed ? 'var(--status-critical)' : 'var(--text-tertiary)' }}
        />
      </div>
      {main}
      <div className="ts-run-failed">
        {failed
          ? `${t('runs.state.failed')} · ${job.error ?? t('runs.analysisError')}`
          : t('runs.state.cancelled')}
      </div>
      <div className="ts-run-eta">—</div>
      <div className="ts-run-time">{relTime(job.finished_ms)}</div>
      <div className="ts-run-action">
        <button className="ts-linkbtn ts-run-link" onClick={() => void retry()}>
          {t('common:actions.retry')}
        </button>
      </div>
    </div>
  );
}

export function AnalysisRuns({ empty, filterable }: { empty?: string; filterable?: boolean }) {
  const { t } = useTranslation('troubleshoot');
  const jobs = useTroubleshootStore((s) => s.jobs);
  const loaded = useTroubleshootStore((s) => s.loaded);
  // Only the dedicated Runs page gets the filter controls; the catalog embeds this as a short
  // summary where three controls above five rows would be noise.
  const columns = useMemo(() => runColumns(t), [t]);
  const labels = useMemo(() => runFilterLabels(t), [t]);
  const [filters, setFilters] = useState<FilterState>(() => defaultFilters(columns));
  const [sheet, setSheet] = useState(false);
  // Component state, not the URL: this list is embedded in two places on the same route (the
  // catalog's summary and the Runs page), and a URL-backed key would be claimed by whichever
  // mounted — the same reason ReportsPage's three tables keep their filters local (Inc.4).
  const narrowed = filterable && isAnyFiltered(columns, filters);
  const shown = useMemo(
    () => (filterable ? jobs.filter(buildPredicate(columns, filters, Date.now())) : jobs),
    [filterable, jobs, columns, filters],
  );
  const counts = useMemo(
    () =>
      Object.fromEntries(
        columns
          .filter((c) => c.filter.kind === 'enum')
          .map((c) => [c.key, facetCounts(jobs, columns, filters, c.key, Date.now())]),
      ),
    [jobs, columns, filters],
  );

  if (jobs.length === 0) {
    return <p className="muted">{loaded ? (empty ?? t('runs.empty')) : t('common:loading')}</p>;
  }
  return (
    <>
      {filterable && (
        <>
          <TableToolbar>
            <MobileFilterButton
              columns={columns}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters
              columns={columns}
              filters={filters}
              onClear={() => setFilters(defaultFilters(columns))}
            />
            <TableSpacer />
            <ResultCount
              shown={shown.length}
              total={narrowed ? jobs.length : undefined}
              noun={t('runs.noun', { count: shown.length })}
            />
          </TableToolbar>
          {/* A run row is a CSS grid with no header row above it, so the controls carry their own
              names instead of sitting under columns that do not exist (ADR-053 Inc.6 decision E). */}
          <FilterBar
            columns={columns}
            labels={labels}
            filters={filters}
            onChange={setFilters}
            counts={counts}
          />
          {sheet && (
            <MobileFilterSheet
              columns={columns}
              labels={labels}
              filters={filters}
              onChange={setFilters}
              counts={counts}
              onClose={() => setSheet(false)}
            />
          )}
        </>
      )}
      {shown.length === 0 ? (
        <p className="muted">{t('common:filter.noMatch')}</p>
      ) : (
        <div className="ts-runs">
          {shown.map((j) => (
            <RunRow key={j.id} job={j} />
          ))}
        </div>
      )}
    </>
  );
}
