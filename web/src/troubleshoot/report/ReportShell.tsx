// SPDX-License-Identifier: AGPL-3.0-only
// The shared chrome behind every Troubleshoot report (ADR-022). Written once so all 15 tools get the
// same lifecycle behaviour and one place fixes a bug: SSE wiring, `?job=` resolution (incl. the
// fallback for a job older than the store's seed window), the wrong-tool guard, the findings load,
// the five lifecycle states, the launch bar, the summary strip, and CSV export.
//
// Everything tool-specific — what the rows say, which viz, which filters, what the strip counts —
// comes from the tool's `ReportDescriptor` (see types.ts). The shell renders `descriptor.Body`.

import { useEffect, useMemo, useState } from 'react';
import { Navigate, useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../../components/ui/PageHeader';
import { Card } from '../../components/ui/Card';
import { Button } from '../../components/ui/Button';
import { Select } from '../../components/ui/Field';
import { api } from '../../services/api';
import { ScopePicker } from '../ScopePicker';
import { allScope, type ScopeValue } from '../scope';
import { TroubleshootToast } from '../TroubleshootToast';
import { relTime } from '../format';
import { reportPathFor, toolById } from '../data';
import { runningCount, useTroubleshootStore } from '../store';
import { useTroubleshootStream } from '../useTroubleshootStream';
import { NoticeRow } from './kit';
import { sigmaFor, toCsv } from './format';
import { buildJobInput } from './jobInput';
import type { ControlState, ReportDescriptor } from './types';
import type { AnalysisFinding, AnalysisJob } from '../../types/api';
import '../troubleshoot.css';
import './report.css';

/** Split the backend's synthetic `kind: 'info'` tier-unavailable rows out of the real findings. */
function splitNotices(rows: AnalysisFinding[]): {
  findings: AnalysisFinding[];
  notices: AnalysisFinding[];
} {
  return {
    findings: rows.filter((f) => f.kind !== 'info'),
    notices: rows.filter((f) => f.kind === 'info'),
  };
}

function Processing({
  pct,
  phase,
  phaseKeys,
  onCancel,
}: {
  pct: number;
  phase: string;
  phaseKeys: string[];
  onCancel: () => void;
}) {
  const { t } = useTranslation('troubleshoot');
  const phases = useMemo(() => phaseKeys.map((k) => t(k)), [t, phaseKeys]);
  const step = Math.min(phases.length - 1, Math.floor((pct / 100) * phases.length));
  return (
    <Card>
      <div className="ts-processing">
        <div className="ts-processing-ring" />
        <div className="ts-processing-title">{t('report.common.processing')}</div>
        <div className="ts-processing-phase">{phase || phases[step]}</div>
        <div className="ts-pbar">
          <div className="ts-pbar-fill" style={{ width: `${pct}%` }} />
        </div>
        <div className="ts-processing-steps">
          {phases.map((p, i) => (
            <div key={p} className={`ts-pstep ${i < step ? 'done' : i === step ? 'active' : ''}`}>
              <span className="ts-pstep-mark">{i < step ? '✓' : i === step ? '▸' : '·'}</span>
              {p}
            </div>
          ))}
        </div>
        <Button className="btn-sm" style={{ marginTop: 8 }} onClick={onCancel}>
          {t('common:actions.cancel')}
        </Button>
      </div>
    </Card>
  );
}

export function ReportShell({ descriptor }: { descriptor: ReportDescriptor }) {
  const { t } = useTranslation('troubleshoot');
  useTroubleshootStream();
  const [params, setParams] = useSearchParams();
  const jobId = params.get('job');
  const jobs = useTroubleshootStore((s) => s.jobs);
  const createJob = useTroubleshootStore((s) => s.createJob);
  const cancelJob = useTroubleshootStore((s) => s.cancelJob);
  const showToast = useTroubleshootStore((s) => s.showToast);

  const tool = toolById(descriptor.tool);
  const toolName = tool ? t(tool.name) : descriptor.tool;
  const { controls } = descriptor;

  // ── Config bar state (the bar is also this tool's launcher) ──
  const [scope, setScope] = useState<ScopeValue>(() => allScope(t));
  const [windowSecs, setWindowSecs] = useState(controls.defaults.windowSecs);
  const [baselineSecs, setBaselineSecs] = useState(controls.defaults.baselineSecs);
  const [sensitivity, setSensitivity] = useState(controls.defaults.sensitivity);

  const shows = (k: string) => controls.controls.includes(k as never);
  const windowLabel =
    controls.windows.find((w) => w.secs === windowSecs)?.labelKey ?? String(windowSecs);

  // ── Job resolution: the store first, then the API for a job older than its seed window ──
  const storeJob = jobId ? jobs.find((j) => j.id === jobId) : undefined;
  const [fetched, setFetched] = useState<AnalysisJob | null>(null);
  const [missing, setMissing] = useState(false);
  useEffect(() => {
    // The store seeds only the most recent jobs, so a bookmarked/older `?job=` isn't there. Without
    // this the page silently rendered its *idle* note, as if nothing had been requested.
    if (!jobId || storeJob || fetched?.id === jobId) return;
    let live = true;
    setMissing(false);
    api
      .getAnalysisJob(jobId)
      .then((j) => {
        if (live) setFetched(j);
      })
      .catch(() => {
        if (live) setMissing(true);
      });
    return () => {
      live = false;
    };
  }, [jobId, storeJob, fetched]);
  // The SSE-backed store row wins once it arrives (it keeps progressing).
  const job = storeJob ?? (fetched?.id === jobId ? fetched : undefined);

  // ── Findings ──
  const [rows, setRows] = useState<AnalysisFinding[]>([]);
  const [loadedFor, setLoadedFor] = useState<string | null>(null);
  useEffect(() => {
    if (job && job.state === 'done' && loadedFor !== job.id) {
      api
        .getAnalysisFindings(job.id)
        .then((f) => {
          setRows(f);
          setLoadedFor(job.id);
        })
        .catch(() => {
          /* transient — SSE will re-trigger */
        });
    }
  }, [job, loadedFor]);
  const { findings, notices } = useMemo(() => splitNotices(rows), [rows]);

  // A `?job=` belonging to another tool would render this body over foreign findings — send it to
  // the report that can actually read it, so every link is self-correcting.
  if (job && job.tool !== descriptor.tool) {
    return <Navigate to={`${reportPathFor(job.tool)}?job=${encodeURIComponent(job.id)}`} replace />;
  }

  const run = async () => {
    const state: ControlState = {
      scopeKind: scope.kind,
      scopeId: scope.id,
      scopeLabel: scope.label,
      windowSecs,
      baselineSecs,
      sensitivity,
      depth: controls.defaults.depth,
    };
    try {
      const j = await createJob(buildJobInput(descriptor, state, t(windowLabel)));
      setRows([]);
      setLoadedFor(null);
      setFetched(null);
      setParams({ job: j.id });
    } catch {
      showToast(t('toast.startFailed'));
    }
  };

  const exportCsv = () => {
    if (!findings.length) {
      showToast(t('report.common.toast.runFirst'));
      return;
    }
    const csv = toCsv(descriptor.csv, findings);
    const url = URL.createObjectURL(new Blob([csv], { type: 'text/csv;charset=utf-8' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = `${descriptor.tool}-findings.csv`;
    a.click();
    URL.revokeObjectURL(url);
  };

  const running = runningCount(jobs);

  return (
    <div>
      <PageHeader
        title={toolName}
        trail={[
          { label: t('nav:sections.troubleshoot'), to: '/troubleshoot' },
          { label: toolName },
        ]}
        note={t(`${descriptor.i18nKey}.note`)}
        actions={
          <>
            <Button className="btn-sm" onClick={() => void run()}>
              {t('actions.rerun')}
            </Button>
            <Button className="btn-sm" onClick={exportCsv}>
              {t('actions.export')}
            </Button>
          </>
        }
      />

      <div className="ts-cfgbar">
        {shows('scope') && (
          <div className="ts-fgroup">
            <label className="ts-flabel" htmlFor="tsr-scope">
              {t('fields.scope')}
            </label>
            <ScopePicker id="tsr-scope" value={scope} onChange={setScope} />
          </div>
        )}
        {shows('window') && (
          <div className="ts-fgroup">
            <label className="ts-flabel" htmlFor="tsr-window">
              {t('fields.window')}
            </label>
            <Select
              id="tsr-window"
              value={String(windowSecs)}
              onChange={(e) => setWindowSecs(Number(e.target.value))}
            >
              {controls.windows.map((w) => (
                <option key={w.secs} value={w.secs}>
                  {t(w.labelKey)}
                </option>
              ))}
            </Select>
          </div>
        )}
        {shows('baseline') && controls.baselines && (
          <div className="ts-fgroup">
            <label className="ts-flabel" htmlFor="tsr-baseline">
              {t('fields.baseline')}
            </label>
            <Select
              id="tsr-baseline"
              value={String(baselineSecs)}
              onChange={(e) => setBaselineSecs(Number(e.target.value))}
            >
              {controls.baselines.map((b) => (
                <option key={b.secs} value={b.secs}>
                  {t(b.labelKey)}
                </option>
              ))}
            </Select>
          </div>
        )}
        {shows('sensitivity') && (
          <div className="ts-fgroup" style={{ minWidth: 170 }}>
            <label className="ts-flabel" htmlFor="tsr-sens">
              {t('fields.sensitivity')}
            </label>
            <div className="ts-slider-row">
              <input
                id="tsr-sens"
                className="ts-slider"
                type="range"
                min={1}
                max={5}
                value={sensitivity}
                onChange={(e) => setSensitivity(Number(e.target.value))}
              />
              <span className="ts-slider-val">σ {sigmaFor(sensitivity).toFixed(1)}</span>
            </div>
          </div>
        )}
        <div className="ts-cfgbar-spacer" />
        <Button variant="primary" onClick={() => void run()}>
          {t('actions.runAnalysis')}
        </Button>
      </div>

      {!job && (
        <Card>
          <div className="ts-empty-note">
            {missing
              ? t('report.common.jobMissing')
              : running > 0
                ? t('report.common.idle.running')
                : t('report.common.idle.ready')}
          </div>
        </Card>
      )}

      {job && job.state === 'running' && (
        <Processing
          pct={job.pct}
          phase={job.phase ?? ''}
          phaseKeys={descriptor.phaseKeys}
          onCancel={() => void cancelJob(job.id)}
        />
      )}

      {job && (job.state === 'failed' || job.state === 'cancelled') && (
        <Card>
          <div className="ts-empty-note">
            {t('report.common.terminal', {
              state: t(`runs.state.${job.state}`),
              detail: job.error ? ` · ${job.error}` : '',
            })}
          </div>
        </Card>
      )}

      {job && job.state === 'done' && (
        <>
          <div className="ts-res-summary">
            {descriptor.summary.map((s) => (
              <span key={s.labelKey} className="tsr-stat-wrap">
                {s.separatorBefore && <span className="ts-res-sep" />}
                <span className="ts-res-stat">
                  <span className={s.tone ? `ts-res-num ${s.tone}` : 'ts-res-num'}>
                    {s.value(findings)}
                  </span>
                  <span className="ts-res-cap">{t(s.labelKey)}</span>
                </span>
              </span>
            ))}
            <div className="ts-res-meta">
              {t('report.common.finished', { time: relTime(job.finished_ms) })}
              <br />
              {job.summary ?? ''}
            </div>
          </div>

          <NoticeRow notices={notices} />

          <descriptor.Body job={job} findings={findings} />
        </>
      )}

      <TroubleshootToast />
    </div>
  );
}
