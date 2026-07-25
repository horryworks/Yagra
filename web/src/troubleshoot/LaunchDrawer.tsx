// SPDX-License-Identifier: AGPL-3.0-only
// Launch drawer (handoff §3): configure and submit any tool as a real background job (ADR-022).
// Scope (All nodes / a group / a single node via ScopePicker), Time window / Depth / "When done"
// (segmented), and a
// Sensitivity slider for ML/Anomaly. On submit it POSTs the job, closes, and toasts (with a
// View → link to the report for tools that have one). The created row appears in Analysis runs
// and progresses over SSE.

import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '../components/ui/Button';
import { Segmented } from './Segmented';
import { ScopePicker } from './ScopePicker';
import { allScope, type ScopeValue } from './scope';
import { METHODS, toolById, type Tool, reportPathFor } from './data';
import { useTroubleshootStore } from './store';
import type { AnalysisJobInput } from '../types/api';

/** Map the 1–5 sensitivity slider to a σ threshold (looser = higher σ = fewer flags). */
function sigmaFor(slider: number): number {
  return 4.5 - 0.5 * slider;
}

const BASELINE_SECS = 14 * 86_400;

export function LaunchDrawer() {
  const { t } = useTranslation('troubleshoot');
  const openToolId = useTroubleshootStore((s) => s.openToolId);
  const closeDrawer = useTroubleshootStore((s) => s.closeDrawer);
  const createJob = useTroubleshootStore((s) => s.createJob);
  const showToast = useTroubleshootStore((s) => s.showToast);

  const WINDOWS = useMemo(
    () => [
      { value: '86400', label: t('launch.windows.h24') },
      { value: '604800', label: t('launch.windows.d7') },
      { value: '2592000', label: t('launch.windows.d30') },
      { value: '7776000', label: t('launch.windows.d90') },
    ],
    [t],
  );
  const DEPTHS = useMemo(
    () => [
      { value: 'quick', label: t('launch.depths.quick') },
      { value: 'standard', label: t('launch.depths.standard') },
      { value: 'exhaustive', label: t('launch.depths.exhaustive') },
    ],
    [t],
  );
  const NOTIFY = useMemo(
    () => [
      { value: 'notify', label: t('launch.notify.notify') },
      { value: 'silent', label: t('launch.notify.silent') },
    ],
    [t],
  );
  const SENS_LABELS = useMemo(
    () => [
      t('launch.sens.veryLoose'),
      t('launch.sens.loose'),
      t('launch.sens.balanced'),
      t('launch.sens.strict'),
      t('launch.sens.veryStrict'),
    ],
    [t],
  );

  // Mirror the selected tool so its content survives the slide-out after openToolId clears.
  const [tool, setTool] = useState<Tool | null>(null);
  const [scope, setScope] = useState<ScopeValue>(() => allScope(t));
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
      const picked = toolById(openToolId);
      if (picked) {
        setTool(picked);
        setScope(allScope(t));
        setWindowVal('604800');
        setDepth('standard');
        setSensitivity(3);
        setNotify('notify');
      }
    }
  }, [openToolId, t]);

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
        t('launch.toast.started', { name: t(tool.name) }),
        `${reportPathFor(tool.id)}?job=${job.id}`,
      );
    } catch {
      showToast(t('toast.startFailed'));
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
        aria-label={tool ? t('launch.aria.configureTool', { name: t(tool.name) }) : t('launch.aria.configure')}
      >
        {tool && (
          <>
            <div className="ts-drawer-head">
              <div className="ts-drawer-mono">{tool.mono}</div>
              <div>
                <div className="ts-drawer-title">{t(tool.name)}</div>
                <div className="ts-drawer-sub">
                  {t('launch.sub', { method: t(METHODS[tool.method].label) })}
                </div>
              </div>
              <button className="ts-drawer-x" onClick={closeDrawer} aria-label={t('common:actions.close')}>
                ×
              </button>
            </div>

            <div className="ts-drawer-body">
              <div className="ts-fgroup">
                <label className="ts-flabel" htmlFor="ts-drawer-scope">
                  {t('fields.scope')}
                </label>
                <ScopePicker
                  id="ts-drawer-scope"
                  className="ts-field-full"
                  value={scope}
                  onChange={setScope}
                />
                <span className="ts-fhint">{t(tool.scope)}</span>
              </div>

              <div className="ts-fgroup">
                <span className="ts-flabel">{t('fields.timeWindow')}</span>
                <Segmented
                  options={WINDOWS}
                  value={windowVal}
                  onChange={setWindowVal}
                  ariaLabel={t('fields.timeWindow')}
                />
              </div>

              <div className="ts-fgroup">
                <span className="ts-flabel">{t('fields.depth')}</span>
                <Segmented options={DEPTHS} value={depth} onChange={setDepth} ariaLabel={t('fields.depth')} />
                <span className="ts-fhint">{t('launch.depthHint')}</span>
              </div>

              {showSensitivity && (
                <div className="ts-fgroup">
                  <label className="ts-flabel" htmlFor="ts-drawer-sens">
                    {t('fields.sensitivity')}
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
                <span className="ts-flabel">{t('fields.whenDone')}</span>
                <Segmented
                  options={NOTIFY}
                  value={notify}
                  onChange={setNotify}
                  ariaLabel={t('fields.whenDone')}
                />
              </div>
            </div>

            <div className="ts-drawer-foot">
              <span className="ts-est">{t('launch.est', { est: t(tool.est) })}</span>
              <Button onClick={closeDrawer}>{t('common:actions.cancel')}</Button>
              <Button variant="primary" onClick={() => void submit()} disabled={submitting}>
                {submitting ? t('actions.starting') : t('actions.runAnalysis')}
              </Button>
            </div>
          </>
        )}
      </aside>
    </>
  );
}
