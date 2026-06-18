// Analysis runs — the async-jobs list (ADR-022), now backed by the real jobs API + SSE. One
// CSS-grid row per job in its lifecycle state:
//  • running   — spinner + live progress bar + phase + %; Cancel
//  • done      — green dot + result summary + relative time; View → (report, if one exists)
//  • failed    — red dot + reason; Retry (re-runs with the same config)
//  • cancelled — grey dot + "cancelled"; Retry
// Progress and terminal states arrive over SSE (store.upsertJob); no client-side faking.

import { useNavigate } from 'react-router-dom';
import { useTroubleshootStore } from './store';
import { toolById } from './data';
import { relTime, inputFromJob } from './format';
import type { AnalysisJob } from '../types/api';

function RunRow({ job }: { job: AnalysisJob }) {
  const navigate = useNavigate();
  const cancelJob = useTroubleshootStore((s) => s.cancelJob);
  const createJob = useTroubleshootStore((s) => s.createJob);
  const showToast = useTroubleshootStore((s) => s.showToast);

  const tool = toolById(job.tool);
  const name = tool?.name ?? job.tool;
  const reportPath = tool?.reportPath;

  const view = () => {
    if (reportPath) navigate(`${reportPath}?job=${encodeURIComponent(job.id)}`);
    else showToast(`A report screen for ${name} is coming soon.`);
  };

  const retry = async () => {
    try {
      const fresh = await createJob(inputFromJob(job));
      showToast(`${name} re-queued.`, reportPath ? `${reportPath}?job=${fresh.id}` : undefined);
    } catch {
      showToast('Could not re-queue the analysis.');
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
          <span className="ts-run-phase">{job.phase ?? 'Running…'}</span>
        </div>
        <div className="ts-run-eta">{pct}%</div>
        <div className="ts-run-time">{relTime(job.created_ms)}</div>
        <div className="ts-run-action">
          <button className="ts-linkbtn ts-run-link" onClick={() => void cancelJob(job.id)}>
            Cancel
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
        <div className="ts-run-findings">{job.summary ?? `${job.finding_count} findings`}</div>
        <div className="ts-run-eta">done</div>
        <div className="ts-run-time">{relTime(job.finished_ms)}</div>
        <div className="ts-run-action">
          <button className="ts-linkbtn ts-run-link" onClick={view}>
            View →
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
        {failed ? `failed · ${job.error ?? 'analysis error'}` : 'cancelled'}
      </div>
      <div className="ts-run-eta">—</div>
      <div className="ts-run-time">{relTime(job.finished_ms)}</div>
      <div className="ts-run-action">
        <button className="ts-linkbtn ts-run-link" onClick={() => void retry()}>
          Retry
        </button>
      </div>
    </div>
  );
}

export function AnalysisRuns({ empty = 'No analysis runs yet.' }: { empty?: string }) {
  const jobs = useTroubleshootStore((s) => s.jobs);
  const loaded = useTroubleshootStore((s) => s.loaded);
  if (jobs.length === 0) {
    return <p className="muted">{loaded ? empty : 'Loading…'}</p>;
  }
  return (
    <div className="ts-runs">
      {jobs.map((j) => (
        <RunRow key={j.id} job={j} />
      ))}
    </div>
  );
}
