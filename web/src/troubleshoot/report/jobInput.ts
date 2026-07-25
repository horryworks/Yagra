// SPDX-License-Identifier: AGPL-3.0-only
// Composing a report's launch request.
//
// Split out of ReportShell as a pure module (no React, no CSS) so it can be unit-tested directly:
// `AnalysisJobInput` is one-size-fits-all, so **hiding a control does not remove its field from the
// POST body**. The engines apply `.max()` floors to `window_secs`/`baseline_secs`, so a tool whose
// bar has no baseline picker must still send a sane baseline — otherwise the same tool would quietly
// produce different results from a report than from the launch drawer.

import { sigmaFor } from './format';
import type { ControlState, ReportDescriptor } from './types';
import type { AnalysisJobInput } from '../../types/api';

/**
 * Build the launch request for a report's config bar. Fields whose control this tool hides are
 * filled from `controls.defaults`; `sensitivity` is converted from the 1–5 slider position to σ
 * (never passed through raw).
 */
export function buildJobInput(
  descriptor: ReportDescriptor,
  state: ControlState,
  /** The already-localized window label, appended to the scope for the runs list. */
  windowLabel: string,
): AnalysisJobInput {
  return {
    tool: descriptor.tool,
    scope_kind: state.scopeKind,
    scope_id: state.scopeId,
    scope_label: `${state.scopeLabel} · ${windowLabel}`,
    window_secs: state.windowSecs,
    baseline_secs: state.baselineSecs,
    sensitivity: sigmaFor(state.sensitivity),
    depth: state.depth,
    family: 'all',
    notify: true,
  };
}

/** The initial control state for a tool, straight from its declared defaults. */
export function initialControlState(
  descriptor: ReportDescriptor,
  scope: { kind: 'all' | 'group' | 'node'; id: string | null; label: string },
): ControlState {
  const d = descriptor.controls.defaults;
  return {
    scopeKind: scope.kind,
    scopeId: scope.id,
    scopeLabel: scope.label,
    windowSecs: d.windowSecs,
    baselineSecs: d.baselineSecs,
    sensitivity: d.sensitivity,
    depth: d.depth,
  };
}
