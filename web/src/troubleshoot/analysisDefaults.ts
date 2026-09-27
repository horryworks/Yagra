// SPDX-License-Identifier: AGPL-3.0-only
// The quick-run defaults for a Troubleshoot analysis: the window, baseline and σ a run gets when
// the operator does not open the drawer at all — and the one list of time windows every launcher
// offers (ADR-184 increment 6).
//
// The windows were written out in four files before this module owned them, and they had already
// drifted: the drawer, the schedule form and the quick run defaulted to 7 days, while the re-run of
// an old job that recorded no window fell back to 24 hours. `analysisDefaults.test.ts` now fails
// any other file under `troubleshoot/` that spells one of these numbers itself.
//
// ⚠️ The report config bar (`report/registry.tsx`) keeps a vocabulary of its own on purpose —
// `report.common.windows` has 1 h / 6 h and no 90 d, and each report picks its own default — so it
// is not a copy of this list and is not folded into it.
//
// This is what stayed behind when `ScopePicker` and its `ScopeValue` moved to
// `components/ScopePicker/` — the scope question is asked by Alerts ▸ History too, but "what does a
// capacity run default its baseline to" is analysis and nothing else.
//
// i18n: `defaultAnalysisInput` takes the caller's `t` rather than resolving at module load, so the
// human `scope_label` follows the active language.

import type { TFunction } from 'i18next';
import type { AnalysisJobInput, AnalysisToolKey } from '../types/api';

/**
 * The time windows an analysis can be run over, shortest first. The label keys live in the
 * `troubleshoot` namespace; `i18nEnumKeys.test.ts` demands each one in EN and JA.
 */
export const ANALYSIS_WINDOWS = [
  { secs: 86_400, labelKey: 'launch.windows.h24' },
  { secs: 604_800, labelKey: 'launch.windows.d7' },
  { secs: 2_592_000, labelKey: 'launch.windows.d30' },
  { secs: 7_776_000, labelKey: 'launch.windows.d90' },
] as const;

export type AnalysisWindowSecs = (typeof ANALYSIS_WINDOWS)[number]['secs'];

/** The window a run gets when nobody picks one. Typed as a member, so it cannot leave the list. */
export const DEFAULT_WINDOW_SECS: AnalysisWindowSecs = 604_800;

/** Baseline lookback: fixed, no launcher offers a choice. */
export const BASELINE_SECS = 14 * 86_400;

/** σ threshold matching the drawer's centre slider (balanced). */
export const DEFAULT_SIGMA = 3.0;

const LABEL_KEYS = Object.fromEntries(
  ANALYSIS_WINDOWS.map((w) => [w.secs, w.labelKey]),
) as Record<AnalysisWindowSecs, string>;

/** Whether a stored or submitted number is one of the offered windows. */
export function isAnalysisWindow(secs: number): secs is AnalysisWindowSecs {
  return ANALYSIS_WINDOWS.some((w) => w.secs === secs);
}

/**
 * The `troubleshoot` label key for a window. A value that is not one of the offered windows — a
 * schedule written through the API, say — is labelled as the default window, which is what the
 * schedule editor did before this module existed.
 */
export function analysisWindowLabelKey(secs: number): string {
  return LABEL_KEYS[isAnalysisWindow(secs) ? secs : DEFAULT_WINDOW_SECS];
}

/** A "quick run" job input: every node, standard defaults — no configuration step. */
export function defaultAnalysisInput(tool: AnalysisToolKey, t: TFunction): AnalysisJobInput {
  const windowLabel = t(analysisWindowLabelKey(DEFAULT_WINDOW_SECS), { ns: 'troubleshoot' });
  return {
    tool,
    scope_kind: 'all',
    scope_id: null,
    scope_label: `${t('common:scope.all')} · ${windowLabel}`,
    window_secs: DEFAULT_WINDOW_SECS,
    baseline_secs: BASELINE_SECS,
    sensitivity: DEFAULT_SIGMA,
    depth: 'standard',
    family: 'all',
    notify: true,
  };
}
