// Launch drawer (handoff §3): configure and submit any tool as a real background job (ADR-022).
// Scope (All nodes / a group / a single node via ScopePicker), Time window / Depth / "When done"
// (segmented), and a
// Sensitivity slider for ML/Anomaly. On submit it POSTs the job, closes, and toasts (with a
// View → link to the report for tools that have one). The created row appears in Analysis runs
// and progresses over SSE.

import { useEffect, useRef, useState } from 'react';
import { Button } from '../components/ui/Button';
import { Segmented } from './Segmented';
import { ScopePicker } from './ScopePicker';
import { ALL_SCOPE, type ScopeValue } from './scope';
import { METHODS, toolById, type Tool } from './data';
import { useTroubleshootStore } from './store';
import type { AnalysisJobInput } from '../types/api';

const WINDOWS = [
  { value: '86400', label: '24 h' },
  { value: '604800', label: '7 d' },
  { value: '2592000', label: '30 d' },
  { value: '7776000', label: '90 d' },
];
const DEPTHS = [
  { value: 'quick', label: 'Quick' },
  { value: 'standard', label: 'Standard' },
  { value: 'exhaustive', label: 'Exhaustive' },
];
const NOTIFY = [
  { value: 'notify', label: 'Notify me' },
  { value: 'silent', label: 'Run silently' },
];
const SENS_LABELS = ['very loose', 'loose', 'balanced', 'strict', 'very strict'];

/** Map the 1–5 sensitivity slider to a σ threshold (looser = higher σ = fewer flags). */
function sigmaFor(slider: number): number {
  return 4.5 - 0.5 * slider;
}

const BASELINE_SECS = 14 * 86_400;

export function LaunchDrawer() {
  const openToolId = useTroubleshootStore((s) => s.openToolId);
  const closeDrawer = useTroubleshootStore((s) => s.closeDrawer);
  const createJob = useTroubleshootStore((s) => s.createJob);
  const showToast = useTroubleshootStore((s) => s.showToast);

  // Mirror the selected tool so its content survives the slide-out after openToolId clears.
  const [tool, setTool] = useState<Tool | null>(null);
  const [scope, setScope] = useState<ScopeValue>(ALL_SCOPE);
  const [windowVal, setWindowVal] = useState('604800');
  const [depth, setDepth] = useState('standard');
  const [sensitivity, setSensitivity] = useState(3);
  const [notify, setNotify] = useState('notify');
  const [submitting, setSubmitting] = useState(false);

  const open = openToolId != null;
  const closeRef = useRef(closeDrawer);
  closeRef.current = closeDrawer;

  useEffect(() => {
    if (openToolId) {
      const t = toolById(openToolId);
      if (t) {
        setTool(t);
        setScope(ALL_SCOPE);
        setWindowVal('604800');
        setDepth('standard');
        setSensitivity(3);
        setNotify('notify');
      }
    }
  }, [openToolId]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeRef.current();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [open]);

  const showSensitivity = tool?.method === 'ml';

  const submit = async () => {
    if (!tool || submitting) return;
    const windowLabel = WINDOWS.find((w) => w.value === windowVal)?.label ?? windowVal;
    const input: AnalysisJobInput = {
      tool: tool.id,
      scope_kind: scope.kind,
      scope_id: scope.id,
      scope_label: `${scope.label} · ${windowLabel}`,
      window_secs: Number(windowVal),
      baseline_secs: BASELINE_SECS,
      sensitivity: sigmaFor(sensitivity),
      depth,
      family: 'all',
      notify: notify === 'notify',
    };
    setSubmitting(true);
    try {
      const job = await createJob(input);
      closeDrawer();
      showToast(
        `${tool.name} started — running in background.`,
        tool.reportPath ? `${tool.reportPath}?job=${job.id}` : undefined,
      );
    } catch {
      showToast('Could not start the analysis.');
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <>
      <div className={open ? 'ts-scrim open' : 'ts-scrim'} onClick={closeDrawer} aria-hidden />
      <aside
        className={open ? 'ts-drawer open' : 'ts-drawer'}
        aria-hidden={!open}
        role="dialog"
        aria-label={tool ? `Configure ${tool.name}` : 'Configure analysis'}
      >
        {tool && (
          <>
            <div className="ts-drawer-head">
              <div className="ts-drawer-mono">{tool.mono}</div>
              <div>
                <div className="ts-drawer-title">{tool.name}</div>
                <div className="ts-drawer-sub">
                  {METHODS[tool.method].label} · configure &amp; run
                </div>
              </div>
              <button className="ts-drawer-x" onClick={closeDrawer} aria-label="Close">
                ×
              </button>
            </div>

            <div className="ts-drawer-body">
              <div className="ts-fgroup">
                <label className="ts-flabel" htmlFor="ts-drawer-scope">
                  Scope
                </label>
                <ScopePicker
                  id="ts-drawer-scope"
                  className="ts-field-full"
                  value={scope}
                  onChange={setScope}
                />
                <span className="ts-fhint">{tool.scope}</span>
              </div>

              <div className="ts-fgroup">
                <span className="ts-flabel">Time window</span>
                <Segmented
                  options={WINDOWS}
                  value={windowVal}
                  onChange={setWindowVal}
                  ariaLabel="Time window"
                />
              </div>

              <div className="ts-fgroup">
                <span className="ts-flabel">Depth</span>
                <Segmented options={DEPTHS} value={depth} onChange={setDepth} ariaLabel="Depth" />
                <span className="ts-fhint">
                  Exhaustive scans more nodes and series — slower, more thorough.
                </span>
              </div>

              {showSensitivity && (
                <div className="ts-fgroup">
                  <label className="ts-flabel" htmlFor="ts-drawer-sens">
                    Sensitivity
                  </label>
                  <div className="ts-slider-row">
                    <input
                      id="ts-drawer-sens"
                      className="ts-slider"
                      type="range"
                      min={1}
                      max={5}
                      value={sensitivity}
                      onChange={(e) => setSensitivity(Number(e.target.value))}
                    />
                    <span className="ts-slider-val">{SENS_LABELS[sensitivity - 1]}</span>
                  </div>
                </div>
              )}

              <div className="ts-fgroup">
                <span className="ts-flabel">When done</span>
                <Segmented
                  options={NOTIFY}
                  value={notify}
                  onChange={setNotify}
                  ariaLabel="When done"
                />
              </div>
            </div>

            <div className="ts-drawer-foot">
              <span className="ts-est">est. {tool.est}</span>
              <Button onClick={closeDrawer}>Cancel</Button>
              <Button variant="primary" onClick={() => void submit()} disabled={submitting}>
                {submitting ? 'Starting…' : 'Run analysis'}
              </Button>
            </div>
          </>
        )}
      </aside>
    </>
  );
}
