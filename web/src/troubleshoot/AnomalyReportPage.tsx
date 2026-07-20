// SPDX-License-Identifier: AGPL-3.0-only
// Anomaly Detection — report (handoff §4) + live processing (§5), now driven by a real job
// (ADR-022). The selected job is in `?job=<id>`; its state/progress arrive over SSE and its
// findings load from the API when it completes. The config bar launches a new anomaly job (real
// scope/window/baseline/sensitivity), then the page shows the processing state until it lands and
// renders the findings. Kind-filter chips + sort re-render the list client-side.

import { useEffect, useMemo, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Select } from '../components/ui/Field';
import { api } from '../services/api';
import { KINDS, kindMeta, type Kind } from './data';
import { runningCount, useTroubleshootStore } from './store';
import { useTroubleshootStream } from './useTroubleshootStream';
import { ScopePicker } from './ScopePicker';
import { allScope, type ScopeValue } from './scope';
import { AnomalyChart } from './AnomalyChart';
import { TroubleshootToast } from './TroubleshootToast';
import { relTime } from './format';
import type { AnalysisFinding, AnomalyDetail } from '../types/api';
import './troubleshoot.css';

/** i18next keys for the four processing phases (resolved with `t()` where they render). */
const PHASE_KEYS = [
  'anomaly.phases.baseline',
  'anomaly.phases.fit',
  'anomaly.phases.score',
  'anomaly.phases.rank',
];

/** 1–5 sensitivity slider → σ threshold (looser = higher σ). */
function sigmaFor(slider: number): number {
  return 4.5 - 0.5 * slider;
}

function AnomalyCard({ finding }: { finding: AnalysisFinding }) {
  const { t } = useTranslation('troubleshoot');
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
        <span className="ts-anom-score-cap">{t('anomaly.card.score')}</span>
      </div>
      <div className="ts-anom-id">
        <span className="ts-anom-node">{finding.node_name}</span>
        <span className="ts-anom-metric">{finding.metric}</span>
        <span className="ts-anom-kind">
          <span className="ts-anom-kind-dot" style={{ background: meta.color }} />
          {t(meta.label)}
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
  const { t } = useTranslation('troubleshoot');
  const phases = useMemo(() => PHASE_KEYS.map((k) => t(k)), [t]);
  const step = Math.min(phases.length - 1, Math.floor(pct / 25));
  return (
    <Card>
      <div className="ts-processing">
        <div className="ts-processing-ring" />
        <div className="ts-processing-title">{t('anomaly.processing.title')}</div>
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

export function AnomalyReportPage() {
  const { t } = useTranslation('troubleshoot');
  useTroubleshootStream();
  const [params, setParams] = useSearchParams();
  const jobId = params.get('job');
  const jobs = useTroubleshootStore((s) => s.jobs);
  const createJob = useTroubleshootStore((s) => s.createJob);
  const cancelJob = useTroubleshootStore((s) => s.cancelJob);
  const showToast = useTroubleshootStore((s) => s.showToast);

  const WINDOWS = useMemo(
    () => [
      { value: '86400', label: t('anomaly.windows.d1') },
      { value: '604800', label: t('anomaly.windows.d7') },
      { value: '2592000', label: t('anomaly.windows.d30') },
    ],
    [t],
  );
  const BASELINES = useMemo(
    () => [
      { value: String(14 * 86_400), label: t('anomaly.baselines.d14') },
      { value: String(30 * 86_400), label: t('anomaly.baselines.d30') },
    ],
    [t],
  );

  // Config bar state.
  const [scope, setScope] = useState<ScopeValue>(() => allScope(t));
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
      showToast(t('toast.startFailed'));
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
          {t('fields.scope')}
        </label>
        <ScopePicker id="ts-cfg-scope" value={scope} onChange={setScope} />
      </div>
      <div className="ts-fgroup">
        <label className="ts-flabel" htmlFor="ts-cfg-window">
          {t('fields.window')}
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
          {t('fields.baseline')}
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
          {t('fields.sensitivity')}
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
        {t('actions.runAnalysis')}
      </Button>
    </div>
  );

  return (
    <div>
      <PageHeader
        title={t('tools.anomaly.name')}
        trail={[{ label: t('nav:sections.troubleshoot'), to: '/troubleshoot' }, { label: t('tools.anomaly.name') }]}
        note={t('anomaly.note')}
        actions={
          <>
            <Button className="btn-sm" onClick={() => void run()}>
              {t('actions.rerun')}
            </Button>
            <Button
              className="btn-sm"
              onClick={() =>
                showToast(
                  findings.length ? t('anomaly.toast.exportSoon') : t('anomaly.toast.runFirst'),
                )
              }
            >
              {t('actions.export')}
            </Button>
          </>
        }
      />

      {configBar}

      {!job && (
        <Card>
          <div className="ts-empty-note">
            {running > 0 ? t('anomaly.empty.running') : t('anomaly.empty.idle')}
          </div>
        </Card>
      )}

      {job && job.state === 'running' && (
        <Processing pct={job.pct} phase={job.phase ?? ''} onCancel={() => void cancelJob(job.id)} />
      )}

      {job && (job.state === 'failed' || job.state === 'cancelled') && (
        <Card>
          <div className="ts-empty-note">
            {t('anomaly.terminalNote', {
              state: t(`runs.state.${job.state}`),
              detail: job.error ? ` · ${job.error}` : '',
            })}
          </div>
        </Card>
      )}

      {job && job.state === 'done' && (
        <>
          <div className="ts-res-summary">
            <div className="ts-res-stat">
              <span className="ts-res-num crit">{crit}</span>
              <span className="ts-res-cap">{t('anomaly.summary.critical')}</span>
            </div>
            <div className="ts-res-stat">
              <span className="ts-res-num warn">{warn}</span>
              <span className="ts-res-cap">{t('anomaly.summary.warning')}</span>
            </div>
            <div className="ts-res-stat">
              <span className="ts-res-num">{findings.length}</span>
              <span className="ts-res-cap">{t('anomaly.summary.total')}</span>
            </div>
            <div className="ts-res-sep" />
            <div className="ts-res-stat">
              <span className="ts-res-num">{nodes}</span>
              <span className="ts-res-cap">{t('anomaly.summary.nodes')}</span>
            </div>
            <div className="ts-res-meta">
              {t('anomaly.summary.finished', { time: relTime(job.finished_ms) })}
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
                {t('anomaly.filters.allKinds')}
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
                  {t(KINDS[k].label)}
                </button>
              ))}
            </div>
            <div className="ts-cfgbar-spacer" />
            <label className="ts-sort-label" htmlFor="ts-anom-sort">
              {t('anomaly.sort.label')}
            </label>
            <Select
              id="ts-anom-sort"
              value={sort}
              onChange={(e) => setSort(e.target.value as 'score' | 'node')}
            >
              <option value="score">{t('anomaly.sort.byScore')}</option>
              <option value="node">{t('anomaly.sort.byNode')}</option>
            </Select>
          </div>

          <div className="ts-anoms">
            {list.length ? (
              list.map((f) => <AnomalyCard key={f.id} finding={f} />)
            ) : (
              <div className="ts-empty-note">
                {findings.length ? t('anomaly.list.emptyKind') : t('anomaly.list.emptyAll')}
              </div>
            )}
          </div>
        </>
      )}

      <TroubleshootToast />
    </div>
  );
}
