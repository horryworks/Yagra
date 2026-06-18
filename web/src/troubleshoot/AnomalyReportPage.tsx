// Anomaly Detection — report (handoff §4) + processing state (§5). Config bar (scope / window /
// baseline / metric families / sensitivity + Run) → results summary → kind-filter chips + sort →
// a finding card per anomaly (score, identity, band chart, when/duration). "Re-run" / "Run
// analysis" shows the centered processing state (advancing 4-step checklist) and lands back on
// the report with a completion toast. Filters and sort re-render the list client-side.

import { useEffect, useMemo, useState } from 'react';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Select } from '../components/ui/Field';
import { ANOMS, KINDS, type Anomaly, type Kind } from './data';
import { useTroubleshootStore } from './store';
import { AnomalyChart } from './AnomalyChart';
import { TroubleshootToast } from './TroubleshootToast';
import './troubleshoot.css';

const PHASES = [
  'Fetching 14 d baseline (412 series)…',
  'Fitting per-metric models…',
  'Scoring residuals…',
  'Ranking & classifying findings…',
];

const TRAIL = [{ label: 'Troubleshoot', to: '/troubleshoot' }, { label: 'Anomaly Detection' }];

function AnomalyCard({ anomaly }: { anomaly: Anomaly }) {
  const kind = KINDS[anomaly.kind];
  return (
    <div className={`ts-anom sev-${anomaly.sev}`}>
      <div className="ts-anom-score">
        <span className="ts-anom-score-num">{anomaly.score}</span>
        <span className="ts-anom-score-cap">score</span>
      </div>
      <div className="ts-anom-id">
        <span className="ts-anom-node">{anomaly.node}</span>
        <span className="ts-anom-metric">{anomaly.metric}</span>
        <span className="ts-anom-kind">
          <span className="ts-anom-kind-dot" style={{ background: kind.color }} />
          {kind.label}
        </span>
      </div>
      <div className="ts-anom-chart">
        <AnomalyChart anomaly={anomaly} />
      </div>
      <div className="ts-anom-right">
        <span className="ts-anom-when">{anomaly.when}</span>
        <span className="ts-anom-dur">{anomaly.dur}</span>
      </div>
    </div>
  );
}

function Processing({ onCancel }: { onCancel: () => void }) {
  const showToast = useTroubleshootStore((s) => s.showToast);
  const [pct, setPct] = useState(0);
  const [step, setStep] = useState(0);

  useEffect(() => {
    let p = 0;
    let doneTimer: ReturnType<typeof setTimeout> | undefined;
    const id = setInterval(() => {
      p = Math.min(100, p + 4 + Math.random() * 6);
      setPct(p);
      setStep(Math.min(PHASES.length - 1, Math.floor(p / 25)));
      if (p >= 100) {
        clearInterval(id);
        doneTimer = setTimeout(() => {
          onCancel();
          showToast('Analysis complete · 23 anomalies found.');
        }, 400);
      }
    }, 520);
    return () => {
      clearInterval(id);
      if (doneTimer) clearTimeout(doneTimer);
    };
  }, [onCancel, showToast]);

  return (
    <div>
      <PageHeader title="Anomaly Detection" trail={TRAIL} />
      <Card>
        <div className="ts-processing">
          <div className="ts-processing-ring" />
          <div className="ts-processing-title">Running analysis…</div>
          <div className="ts-processing-phase">{PHASES[Math.min(step, PHASES.length - 1)]}</div>
          <div className="ts-pbar">
            <div className="ts-pbar-fill" style={{ width: `${pct}%` }} />
          </div>
          <div className="ts-processing-steps">
            {PHASES.map((p, i) => (
              <div
                key={p}
                className={`ts-pstep ${i < step ? 'done' : i === step ? 'active' : ''}`}
              >
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
      <TroubleshootToast />
    </div>
  );
}

export function AnomalyReportPage() {
  const showToast = useTroubleshootStore((s) => s.showToast);
  const [processing, setProcessing] = useState(false);
  const [filter, setFilter] = useState<'all' | Kind>('all');
  const [sort, setSort] = useState<'score' | 'time'>('score');
  const [sensitivity, setSensitivity] = useState(3);

  const list = useMemo(() => {
    let l = filter === 'all' ? ANOMS.slice() : ANOMS.filter((a) => a.kind === filter);
    if (sort === 'score') l = l.sort((a, b) => b.score - a.score);
    // 'time' keeps the source order (already newest-first).
    return l;
  }, [filter, sort]);

  const crit = ANOMS.filter((a) => a.sev === 'crit').length;
  const warn = ANOMS.filter((a) => a.sev === 'warn').length;
  const nodes = new Set(ANOMS.map((a) => a.node)).size;
  const sigma = (1.5 + sensitivity * 0.5).toFixed(1);

  if (processing) return <Processing onCancel={() => setProcessing(false)} />;

  return (
    <div>
      <PageHeader
        title="Anomaly Detection"
        trail={TRAIL}
        note="Baseline-relative deviations across every collected series. Each finding is scored by how far and how unusually it left its learned envelope."
        actions={
          <>
            <Button className="btn-sm" onClick={() => setProcessing(true)}>
              Re-run
            </Button>
            <Button className="btn-sm" onClick={() => showToast('Exported report to CSV.')}>
              Export
            </Button>
          </>
        }
      />

      <div className="ts-cfgbar">
        <div className="ts-fgroup">
          <label className="ts-flabel" htmlFor="ts-cfg-scope">
            Scope
          </label>
          <Select id="ts-cfg-scope" defaultValue="group">
            <option value="group">group: Matsumoto / core (18)</option>
            <option value="all">all nodes (128)</option>
            <option value="role">role: edge firewalls (9)</option>
          </Select>
        </div>
        <div className="ts-fgroup">
          <label className="ts-flabel" htmlFor="ts-cfg-window">
            Window
          </label>
          <Select id="ts-cfg-window" defaultValue="24h">
            <option value="24h">last 24 h</option>
            <option value="7d">last 7 d</option>
            <option value="30d">last 30 d</option>
          </Select>
        </div>
        <div className="ts-fgroup">
          <label className="ts-flabel" htmlFor="ts-cfg-baseline">
            Baseline
          </label>
          <Select id="ts-cfg-baseline" defaultValue="14d">
            <option value="14d">14 d trailing</option>
            <option value="30d">30 d trailing</option>
            <option value="dow">same-DOW 4 wk</option>
          </Select>
        </div>
        <div className="ts-fgroup">
          <label className="ts-flabel" htmlFor="ts-cfg-families">
            Metric families
          </label>
          <Select id="ts-cfg-families" defaultValue="all">
            <option value="all">all families</option>
            <option value="reach">reachability + interface</option>
            <option value="system">system (cpu/mem/temp)</option>
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
        <Button variant="primary" onClick={() => setProcessing(true)}>
          Run analysis
        </Button>
      </div>

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
          <span className="ts-res-num">{ANOMS.length}</span>
          <span className="ts-res-cap">total</span>
        </div>
        <div className="ts-res-sep" />
        <div className="ts-res-stat">
          <span className="ts-res-num">{nodes}</span>
          <span className="ts-res-cap">nodes</span>
        </div>
        <div className="ts-res-stat">
          <span className="ts-res-num">412</span>
          <span className="ts-res-cap">series scanned</span>
        </div>
        <div className="ts-res-meta">
          finished 8m ago · 2m 11s
          <br />
          baseline 14 d · σ 3.0 · seasonal-adjusted
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
          onChange={(e) => setSort(e.target.value as 'score' | 'time')}
        >
          <option value="score">by score</option>
          <option value="time">by time</option>
        </Select>
      </div>

      <div className="ts-anoms">
        {list.length ? (
          list.map((a) => <AnomalyCard key={`${a.node}-${a.metric}`} anomaly={a} />)
        ) : (
          <div className="ts-empty-note">No anomalies of this kind in the current scope.</div>
        )}
      </div>

      <TroubleshootToast />
    </div>
  );
}
