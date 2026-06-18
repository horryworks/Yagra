// Launch drawer (handoff §3): right-side slide-in panel to configure and submit any tool as a
// background job. Scope (select), Time window / Depth / "When done" (segmented), and a
// Sensitivity slider for ML/Anomaly tools. On submit it prepends a running job to the store,
// closes, and shows a toast (with a View → link when the tool has a report screen).
//
// Driven by the store's `openToolId`. The selected tool is mirrored into local state so its
// content stays put during the close (slide-out) animation. Closes on scrim click / Escape / ×.

import { useEffect, useRef, useState } from 'react';
import { Select } from '../components/ui/Field';
import { Button } from '../components/ui/Button';
import { Segmented } from './Segmented';
import { METHODS, toolById, type Tool } from './data';
import { useTroubleshootStore } from './store';

const WINDOWS = [
  { value: '24h', label: '24 h' },
  { value: '7d', label: '7 d' },
  { value: '30d', label: '30 d' },
  { value: '90d', label: '90 d' },
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

const SCOPES = [
  'group: Matsumoto / core (18 nodes)',
  'all nodes (128)',
  'role: edge firewalls (9)',
  'single node…',
];

export function LaunchDrawer() {
  const openToolId = useTroubleshootStore((s) => s.openToolId);
  const closeDrawer = useTroubleshootStore((s) => s.closeDrawer);
  const addRun = useTroubleshootStore((s) => s.addRun);
  const showToast = useTroubleshootStore((s) => s.showToast);

  // Mirror the selected tool so its content survives the slide-out after openToolId clears.
  const [tool, setTool] = useState<Tool | null>(null);
  const [scope, setScope] = useState(SCOPES[0]);
  const [windowVal, setWindowVal] = useState('7d');
  const [depth, setDepth] = useState('standard');
  const [sensitivity, setSensitivity] = useState(3);
  const [notify, setNotify] = useState('notify');

  const open = openToolId != null;
  const closeRef = useRef(closeDrawer);
  closeRef.current = closeDrawer;

  useEffect(() => {
    if (openToolId) {
      const t = toolById(openToolId);
      if (t) {
        setTool(t);
        // Reset the form to defaults for each freshly-opened tool.
        setScope(SCOPES[0]);
        setWindowVal('7d');
        setDepth('standard');
        setSensitivity(3);
        setNotify('notify');
      }
    }
  }, [openToolId]);

  // Escape closes the drawer while open.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeRef.current();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [open]);

  const showSensitivity = tool?.method === 'ml';
  const windowLabel = WINDOWS.find((w) => w.value === windowVal)?.label ?? windowVal;

  const submit = () => {
    if (!tool) return;
    addRun({
      tool: tool.name,
      mono: tool.mono,
      scope: `${scope} · ${windowLabel}`,
      state: 'running',
      pct: 3,
      phase: 'Queued — fetching history…',
      eta: tool.est,
      started: 'just now',
      reportPath: tool.reportPath,
    });
    closeDrawer();
    showToast(`${tool.name} started — running in background.`, tool.reportPath);
  };

  return (
    <>
      <div
        className={open ? 'ts-scrim open' : 'ts-scrim'}
        onClick={closeDrawer}
        aria-hidden
      />
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
                <div className="ts-drawer-sub">{METHODS[tool.method].label} · configure &amp; run</div>
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
                <Select
                  id="ts-drawer-scope"
                  className="ts-field-full"
                  value={scope}
                  onChange={(e) => setScope(e.target.value)}
                >
                  {SCOPES.map((s) => (
                    <option key={s} value={s}>
                      {s}
                    </option>
                  ))}
                </Select>
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
                  Exhaustive scans more series and longer history — slower, more thorough.
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
              <Button variant="primary" onClick={submit}>
                Run analysis
              </Button>
            </div>
          </>
        )}
      </aside>
    </>
  );
}
