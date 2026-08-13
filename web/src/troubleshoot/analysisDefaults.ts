// SPDX-License-Identifier: AGPL-3.0-only
// The quick-run defaults for a Troubleshoot analysis: the window, baseline and σ a run gets when
// the operator does not open the drawer at all.
//
// This is what stayed behind when `ScopePicker` and its `ScopeValue` moved to
// `components/ScopePicker/` — the scope question is asked by Alerts ▸ History too, but "what does a
// capacity run default its baseline to" is analysis and nothing else.
//
// i18n: `defaultAnalysisInput` takes the caller's `t` rather than resolving at module load, so the
// human `scope_label` follows the active language.

import type { TFunction } from 'i18next';
import type { AnalysisJobInput, AnalysisToolKey } from '../types/api';

/** Quick-run defaults (the split-button "Run on all nodes" path + the drawer's initial state). */
export const DEFAULT_WINDOW_SECS = 7 * 86_400;
export const DEFAULT_BASELINE_SECS = 14 * 86_400;
/** σ threshold matching the drawer's centre slider (balanced). */
export const DEFAULT_SIGMA = 3.0;

/** A "quick run" job input: every node, standard defaults — no configuration step. */
export function defaultAnalysisInput(tool: AnalysisToolKey, t: TFunction): AnalysisJobInput {
  return {
    tool,
    scope_kind: 'all',
    scope_id: null,
    scope_label: `${t('common:scope.all')} · ${t('troubleshoot:launch.windows.d7')}`,
    window_secs: DEFAULT_WINDOW_SECS,
    baseline_secs: DEFAULT_BASELINE_SECS,
    sensitivity: DEFAULT_SIGMA,
    depth: 'standard',
    family: 'all',
    notify: true,
  };
}
