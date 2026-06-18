// Anomaly Detection — report (handoff §4) + live processing (§5), now driven by a real job
// (ADR-022). The selected job is in `?job=<id>`; its state/progress arrive over SSE and its
// findings load from the API when it completes. The config bar launches a new anomaly job (real
// scope/window/baseline/sensitivity), then the page shows the processing state until it lands and
// renders the findings. Kind-filter chips + sort re-render the list client-side.

import { useEffect, useMemo, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Select } from '../components/ui/Field';
import { api } from '../services/api';
import { KINDS, kindMeta, type Kind } from './data';
import { runningCount, useTroubleshootStore } from './store';
import { useTroubleshootStream } from './useTroubleshootStream';
import { useScopeOptions } from './useScopeOptions';
import { AnomalyChart } from './AnomalyChart';
import { TroubleshootToast } from './TroubleshootToast';
import { relTime } from './format';
import type { AnalysisFinding, AnomalyDetail } from '../types/api';
import './troubleshoot.css';

const TRAIL = [{ label: 'Troubleshoot', to: '/troubleshoot' }, { label: 'Anomaly Detection' }];
const WINDOWS = [
  { value: '86400', label: 'last 24 h' },
  { value: '604800', label: 'last 7 d' },
  { value: '2592000', label: 'last 30 d' },
];
const BASELINES = [
  { value: String(14 * 86_400), label: '14 d trailing' },
  { value: String(30 * 86_400), label: '30 d trailing' },
];
const PHASES = [
  'Fetching baseline…',
  'Fitting per-metric models…',
  'Scoring residuals…',
  'Ranking & classifying findings…',
];

/** 1–5 sensitivity slider → σ threshold (looser = higher σ). */
function sigmaFor(slider: number): number {
  return 4.5 - 0.5 * slider;
}

function AnomalyCard({ finding }: { finding: AnalysisFinding }) {
  const meta = kindMeta(finding.kind);
  const detail = finding.detail as AnomalyDetail;
  const sev = (finding.severity === 'crit' || finding.severity === 'warn' ? finding.severity : 'info') as
    | 'crit'
    | 'warn'
    | 'info';
  return (
    <div className={`ts-anom sev-${sev}`}>
      <div className="ts-anom-score">
        <span className="ts-anom-score-num">{Math.round(finding.score)}</span>
        <span className="ts-anom-score-cap">score</span>
      </div>
      <div className="ts-anom-id">
        <span className="ts-anom-node">{finding.node_name}</span>
        <span className="ts-anom-metric">{finding.metric}</span>
        <span className="ts-anom-kind">
          <span className="ts-anom-kind-dot" style={{ background: meta.color }} />
          {meta.label}
        </span>
      </div>
      <div className="ts-anom-chart">
        {detail?.points ? <AnomalyChart detail={detail} severity={sev} /> : null}
      </div>
      <div className="ts-anom-right">
        <span className="ts-anom-when">{finding.when_label}</span>
        <span className="ts-anom-dur">{finding.duration}</span>
      </div>
    </div>
  );
}

function Processing({ pct, phase, onCancel }: { pct: number; phase: string; onCancel: () => void }) {
  const step = Math.min(PHASES.length - 1, Math.floor(pct / 25));
  return (
    <Card>
      <div className="ts-processing">
        <div className="ts-processing-ring" />
        <div className="ts-processing-title">Running analysis…</div>
        <div className="ts-processing-phase">{phase || PHASES[step]}</div>
        <div className="ts-pbar">
          <div className="ts-pbar-fill" style={{ width: `${pct}%` }} />
        </div>
        <div className="ts-processing-steps">
          {PHASES.map((p, i) => (
            <div key={p} className={`ts-pstep ${i < step ? 'done' : i === step ? 'active' : ''}`}>
              <span className="ts-pstep-mark">{i < step ? '✓' : i === step ? '▸' : '·'}</span>
              {p}
            </div>
          ))}
        </div>
        <Button className="btn-sm" style={{ marginTop: 8 }} onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </Card>
  );
}

export function AnomalyReportPage() {
  useTroubleshootStream();
  const [params, setParams] = useSearchParams();
  const jobId = params.get('job');
  const jobs = useTroubleshootStore((s) => s.jobs);
  const createJob = useTroubleshootStore((s) => s.createJob);
  const cancelJob = useTroubleshootStore((s) => s.cancelJob);
  const showToast = useTroubleshootStore((s) => s.showToast);
  const scopes = useScopeOptions();

  // Config bar state.
  const [scopeIdx, setScopeIdx] = useState(0);
  const [windowVal, setWindowVal] = useState('86400');
  const [baselineVal, setBaselineVal] = useState(String(14 * 86_400));
  const [sensitivity, setSensitivity] = useState(3);
  // List controls.
  const [filter, setFilter] = useState<'all' | Kind>('all');
  const [sort, setSort] = useState<'score' | 'node'>('score');

  const job = jobId ? jobs.find((j) => j.id === jobId) : undefined;

  // Load findings once the job is done.
  const [findings, setFindings] = useState<AnalysisFinding[]>([]);
  const [loadedFor, setLoadedFor] = useState<string | null>(null);
  useEffect(() => {
    if (job && job.state === 'done' && loadedFor !== job.id) {
      api
        .getAnalysisFindings(job.id)
        .then((f) => {
          setFindings(f);
          setLoadedFor(job.id);
        })
        .catch(() => {
          /* transient */
        });
    }
  }, [job, loadedFor]);

  const run = async () => {
    const scope = scopes[scopeIdx] ?? scopes[0];
    const windowLabel = WINDOWS.find((w) => w.value === windowVal)?.label ?? windowVal;
    try {
      const j = await createJob({
        tool: 'anomaly',
        scope_kind: scope.kind,
        scope_id: scope.id,
        scope_label: `${scope.label} · ${windowLabel}`,
        window_secs: Number(windowVal),
        baseline_secs: Number(baselineVal),
        sensitivity: sigmaFor(sensitivity),
        depth: 'standard',
        family: 'all',
        notify: true,
      });
      setFindings([]);
      setLoadedFor(null);
      setParams({ job: j.id });
    } catch {
      showToast('Could not start the analysis.');
    }
  };

  const list = useMemo(() => {
    let l = filter === 'all' ? findings.slice() : findings.filter((f) => f.kind === filter);
    if (sort === 'score') l = l.sort((a, b) => b.score - a.score);
    else l = l.sort((a, b) => a.node_name.localeCompare(b.node_name));
    return l;
  }, [findings, filter, sort]);

  const crit = findings.filter((f) => f.severity === 'crit').length;
  const warn = findings.filter((f) => f.severity === 'warn').length;
  const nodes = new Set(findings.map((f) => f.node_id).filter(Boolean)).size;
  const sigma = sigmaFor(sensitivity).toFixed(1);
  const running = runningCount(jobs);

  const configBar = (
    <div className="ts-cfgbar">
      <div className="ts-fgroup">
        <label className="ts-flabel" htmlFor="ts-cfg-scope">
          Scope
        </label>
        <Select
          id="ts-cfg-scope"
          value={String(scopeIdx)}
          onChange={(e) => setScopeIdx(Number(e.target.value))}
        >
          {scopes.map((s, i) => (
            <option key={`${s.kind}-${s.id ?? 'all'}`} value={i}>
              {s.label}
            </option>
          ))}
        </Select>
      </div>
      <div className="ts-fgroup">
        <label className="ts-flabel" htmlFor="ts-cfg-window">
          Window
        </label>
        <Select id="ts-cfg-window" value={windowVal} onChange={(e) => setWindowVal(e.target.value)}>
          {WINDOWS.map((w) => (
            <option key={w.value} value={w.value}>
              {w.label}
            </option>
          ))}
        </Select>
      </div>
      <div className="ts-fgroup">
        <label className="ts-flabel" htmlFor="ts-cfg-baseline">
          Baseline
        </label>
        <Select
          id="ts-cfg-baseline"
          value={baselineVal}
          onChange={(e) => setBaselineVal(e.target.value)}
        >
          {BASELINES.map((b) => (
            <option key={b.value} value={b.value}>
              {b.label}
            </option>
          ))}
        </Select>
      </div>
      <div className="ts-fgroup" style={{ minWidth: 170 }}>
        <label className="ts-flabel" htmlFor="ts-cfg-sens">
          Sensitivity
        </label>
        <div className="ts-slider-row">
          <input
            id="ts-cfg-sens"
            className="ts-slider"
            type="range"
            min={1}
            max={5}
            value={sensitivity}
            onChange={(e) => setSensitivity(Number(e.target.value))}
          />
          <span className="ts-slider-val">σ {sigma}</span>
        </div>
      </div>
      <div className="ts-cfgbar-spacer" />
      <Button variant="primary" onClick={() => void run()}>
        Run analysis
      </Button>
    </div>
  );

  return (
    <div>
      <PageHeader
        title="Anomaly Detection"
        trail={TRAIL}
        note="Baseline-relative deviations across every collected series. Each finding is scored by how far and how unusually it left its learned envelope."
        actions={
          <>
            <Button className="btn-sm" onClick={() => void run()}>
              Re-run
            </Button>
            <Button
              className="btn-sm"
              onClick={() =>
                showToast(
                  findings.length ? 'Export to CSV is coming soon.' : 'Run an analysis first.',
                )
              }
            >
              Export
            </Button>
          </>
        }
      />

      {configBar}

      {!job && (
        <Card>
          <div className="ts-empty-note">
            {running > 0
              ? 'An analysis is running — open it from Analysis runs, or start a new one above.'
              : 'Set the scope and window above, then Run analysis to detect anomalies.'}
          </div>
        </Card>
      )}

      {job && job.state === 'running' && (
        <Processing pct={job.pct} phase={job.phase ?? ''} onCancel={() => void cancelJob(job.id)} />
      )}

      {job && (job.state === 'failed' || job.state === 'cancelled') && (
        <Card>
          <div className="ts-empty-note">
            Analysis {job.state}
            {job.error ? ` · ${job.error}` : ''}. Adjust the scope and run again.
          </div>
        </Card>
      )}

      {job && job.state === 'done' && (
        <>
          <div className="ts-res-summary">
            <div className="ts-res-stat">
              <span className="ts-res-num crit">{crit}</span>
              <span className="ts-res-cap">critical</span>
            </div>
            <div className="ts-res-stat">
              <span className="ts-res-num warn">{warn}</span>
              <span className="ts-res-cap">warning</span>
            </div>
            <div className="ts-res-stat">
              <span className="ts-res-num">{findings.length}</span>
              <span className="ts-res-cap">total</span>
            </div>
            <div className="ts-res-sep" />
            <div className="ts-res-stat">
              <span className="ts-res-num">{nodes}</span>
              <span className="ts-res-cap">nodes</span>
            </div>
            <div className="ts-res-meta">
              finished {relTime(job.finished_ms)}
              <br />
              {job.summary ?? ''}
            </div>
          </div>

          <div className="ts-res-toolbar">
            <div className="ts-chip-row">
              <button
                type="button"
                className={filter === 'all' ? 'ts-chip on' : 'ts-chip'}
                aria-pressed={filter === 'all'}
                onClick={() => setFilter('all')}
              >
                All kinds
              </button>
              {(Object.keys(KINDS) as Kind[]).map((k) => (
                <button
                  key={k}
                  type="button"
                  className={filter === k ? 'ts-chip on' : 'ts-chip'}
                  aria-pressed={filter === k}
                  onClick={() => setFilter(k)}
                >
                  <span className="ts-chip-dot" style={{ background: KINDS[k].color }} />
                  {KINDS[k].label}
                </button>
              ))}
            </div>
            <div className="ts-cfgbar-spacer" />
            <label className="ts-sort-label" htmlFor="ts-anom-sort">
              Sort
            </label>
            <Select
              id="ts-anom-sort"
              value={sort}
              onChange={(e) => setSort(e.target.value as 'score' | 'node')}
            >
              <option value="score">by score</option>
              <option value="node">by node</option>
            </Select>
          </div>

          <div className="ts-anoms">
            {list.length ? (
              list.map((f) => <AnomalyCard key={f.id} finding={f} />)
            ) : (
              <div className="ts-empty-note">
                {findings.length
                  ? 'No anomalies of this kind in the current scope.'
                  : 'No anomalies found — everything is within its learned envelope.'}
              </div>
            )}
          </div>
        </>
      )}

      <TroubleshootToast />
    </div>
  );
}
