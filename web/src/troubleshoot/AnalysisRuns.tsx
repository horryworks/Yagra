// Analysis runs — the async-jobs list (handoff §2). One CSS-grid row per job in three states:
//  • running — spinner + live progress bar + phase + %/ETA + Cancel
//  • done    — green dot + findings summary + relative time + View → (report, if one exists)
//  • failed  — red dot + reason + Retry
// Progress ticks up live (driven by the store's tickProgress on an interval mounted by the page)
// to make the background-job nature legible. Findings are structured data (count + text),
// rendered through JSX — never innerHTML — since result text is untrusted.

import { useNavigate } from 'react-router-dom';
import { useTroubleshootStore } from './store';
import type { Run } from './data';

function RunRow({ run }: { run: Run }) {
  const navigate = useNavigate();
  const cancelRun = useTroubleshootStore((s) => s.cancelRun);
  const retryRun = useTroubleshootStore((s) => s.retryRun);
  const showToast = useTroubleshootStore((s) => s.showToast);

  const view = () => {
    if (run.reportPath) navigate(run.reportPath);
    else showToast(`A report screen for ${run.tool} is coming soon.`);
  };

  if (run.state === 'running') {
    const pct = Math.round(run.pct ?? 0);
    return (
      <div className="ts-run">
        <div className="ts-run-status">
          <div className="ts-run-spin" />
        </div>
        <div className="ts-run-main">
          <span className="ts-run-tool">{run.tool}</span>
          <span className="ts-run-scope">{run.scope}</span>
        </div>
        <div className="ts-run-prog">
          <div className="ts-run-bar">
            <div className="ts-run-bar-fill" style={{ width: `${pct}%` }} />
          </div>
          <span className="ts-run-phase">{run.phase}</span>
        </div>
        <div className="ts-run-eta">
          {pct}%<br />
          {run.eta}
        </div>
        <div className="ts-run-time">{run.started}</div>
        <div className="ts-run-action">
          <button className="ts-linkbtn ts-run-link" onClick={() => cancelRun(run.id)}>
            Cancel
          </button>
        </div>
      </div>
    );
  }

  if (run.state === 'done') {
    return (
      <div className="ts-run">
        <div className="ts-run-status">
          <span className="ts-run-dot" style={{ background: 'var(--status-up)' }} />
        </div>
        <div className="ts-run-main">
          <span className="ts-run-tool">{run.tool}</span>
          <span className="ts-run-scope">{run.scope}</span>
        </div>
        <div className="ts-run-findings">
          {run.findings && (
            <>
              <b>{run.findings.count}</b> {run.findings.text}
            </>
          )}
        </div>
        <div className="ts-run-eta">done</div>
        <div className="ts-run-time">{run.when}</div>
        <div className="ts-run-action">
          <button className="ts-linkbtn ts-run-link" onClick={view}>
            View →
          </button>
        </div>
      </div>
    );
  }

  // failed
  return (
    <div className="ts-run">
      <div className="ts-run-status">
        <span className="ts-run-dot" style={{ background: 'var(--status-critical)' }} />
      </div>
      <div className="ts-run-main">
        <span className="ts-run-tool">{run.tool}</span>
        <span className="ts-run-scope">{run.scope}</span>
      </div>
      <div className="ts-run-failed">failed · {run.err}</div>
      <div className="ts-run-eta">—</div>
      <div className="ts-run-time">{run.when}</div>
      <div className="ts-run-action">
        <button
          className="ts-linkbtn ts-run-link"
          onClick={() => {
            retryRun(run.id);
            showToast('Re-queued analysis.');
          }}
        >
          Retry
        </button>
      </div>
    </div>
  );
}

export function AnalysisRuns({ empty = 'No analysis runs yet.' }: { empty?: string }) {
  const runs = useTroubleshootStore((s) => s.runs);
  if (runs.length === 0) return <p className="muted">{empty}</p>;
  return (
    <div className="ts-runs">
      {runs.map((r) => (
        <RunRow key={r.id} run={r} />
      ))}
    </div>
  );
}
