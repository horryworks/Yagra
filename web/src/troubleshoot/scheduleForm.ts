// SPDX-License-Identifier: AGPL-3.0-only
// Scheduled analyses — the form's judgement, as pure functions.
//
// Here rather than in the modal because Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so a test beside a `.tsx` is a file nothing runs (testing.md).
// The modal is layout; everything that decides what gets POSTed is below.

import type {
  AnalysisSchedule,
  AnalysisScheduleInput,
  AnalysisToolKey,
  Cadence,
} from '../types/api';
import { TOOLS } from './data';
import { sigmaFor } from './report/format';
import type { ScopeValue } from '../components/ScopePicker/scope';

/** Baseline lookback, matching the launch drawer's fixed value. */
export const BASELINE_SECS = 14 * 86_400;

/** The analysis windows the form offers, in seconds. */
export const WINDOW_CHOICES = [86_400, 604_800, 2_592_000, 7_776_000] as const;

/** The form's state. Mirrors the launch drawer's fields plus the cadence. */
export interface ScheduleForm {
  tool: AnalysisToolKey;
  scope: ScopeValue;
  windowSecs: number;
  /** Slider position 1–5, converted to σ on submit. */
  sensitivity: number;
  notify: boolean;
  frequency: Cadence;
  /** 0=Sun … 6=Sat, used when `frequency` is `weekly`. */
  dayOfWeek: number;
  /** 1 … 28, used when `frequency` is `monthly`. */
  dayOfMonth: number;
  atHour: number;
  atMinute: number;
  enabled: boolean;
}

/**
 * A blank form.
 *
 * 03:00 daily, silent. Unattended analyses default to **not** notifying, unlike the launch drawer
 * where someone is waiting for the result — a nightly analysis that pages on every run is how a
 * scheduling feature gets turned off a week after it ships.
 */
export function blankSchedule(scope: ScopeValue): ScheduleForm {
  return {
    tool: 'anomaly',
    scope,
    windowSecs: 604_800,
    sensitivity: 3,
    notify: false,
    frequency: 'daily',
    dayOfWeek: 1,
    dayOfMonth: 1,
    atHour: 3,
    atMinute: 0,
    enabled: true,
  };
}

/** Load an existing schedule back into the form. */
export function formFromSchedule(s: AnalysisSchedule, scope: ScopeValue): ScheduleForm {
  const params = (s.params ?? {}) as Record<string, unknown>;
  const num = (k: string, fallback: number) =>
    typeof params[k] === 'number' ? (params[k] as number) : fallback;
  return {
    // `tool` is a bare string on the wire; an unknown one falls back rather than leaving the
    // picker on a value it cannot render.
    tool: (TOOLS.some((t) => t.id === s.tool) ? s.tool : 'anomaly') as AnalysisToolKey,
    scope,
    windowSecs: num('window_secs', 604_800),
    sensitivity: sliderFor(num('sensitivity', 3)),
    notify: params.notify === true,
    // A cadence this build does not know must not silently become "daily" in an edit form — that
    // would rewrite a monthly schedule to nightly the first time somebody opened it and saved.
    // `unknown` is not selectable, so the modal refuses to save until a real one is chosen.
    frequency: s.frequency,
    dayOfWeek: s.day_of_week ?? 1,
    dayOfMonth: s.day_of_month ?? 1,
    atHour: s.at_hour,
    atMinute: s.at_minute,
    enabled: s.enabled,
  };
}

/**
 * A stored σ back to its 1–5 slider position — the inverse of `report/format.ts::sigmaFor`.
 *
 * Derived from that function rather than restating its scale. A second copy of the slider→σ table
 * would mean an edit form that silently moved the sensitivity of every schedule it opened, and the
 * round trip is what a test can pin (`sigmaFor` stays the single definition).
 */
export function sliderFor(sigma: number): number {
  // sigmaFor(s) = 4.5 - 0.5·s  ⇒  s = (4.5 - σ) / 0.5
  const raw = (4.5 - sigma) / 0.5;
  return Math.min(Math.max(Math.round(raw), 1), 5);
}

/** Every reason the schedule dialog refuses to submit. `as const` so the i18n coverage test can
 *  walk it: the dialog renders `t(`schedule.err.${problem}`)` with no fallback. */
export const SCHEDULE_FORM_PROBLEMS = ['unknown_cadence', 'missing_scope'] as const;

/** Why a schedule cannot be saved. */
export type ScheduleFormProblem = (typeof SCHEDULE_FORM_PROBLEMS)[number];

/** Whether the form can be submitted — the reasons are all things the backend would reject. */
export function scheduleFormError(f: ScheduleForm): ScheduleFormProblem | null {
  if (f.frequency === 'unknown') return 'unknown_cadence';
  // A group/node scope with no id would widen to the whole fleet in the runner.
  if (f.scope.kind !== 'all' && !f.scope.id) return 'missing_scope';
  return null;
}

/**
 * The request body for a form.
 *
 * The day fields are sent only for the cadence that reads them. The backend drops the other one
 * anyway, but sending both means the wire body disagrees with what is stored, and the next `GET`
 * then looks like it lost a field.
 */
export function scheduleBody(f: ScheduleForm, windowLabel: string): AnalysisScheduleInput {
  return {
    tool: f.tool,
    scope_kind: f.scope.kind,
    scope_id: f.scope.id,
    scope_label: `${f.scope.label} · ${windowLabel}`,
    window_secs: f.windowSecs,
    baseline_secs: BASELINE_SECS,
    sensitivity: sigmaFor(f.sensitivity),
    depth: 'standard',
    family: 'all',
    notify: f.notify,
    frequency: f.frequency,
    day_of_week: f.frequency === 'weekly' ? f.dayOfWeek : null,
    day_of_month: f.frequency === 'monthly' ? f.dayOfMonth : null,
    at_hour: f.atHour,
    at_minute: f.atMinute,
    enabled: f.enabled,
  };
}

/**
 * The tools a schedule may name, given whether this deployment has a flow store.
 *
 * A deliberate subset rather than a disabled option: `POST /analysis/schedules` refuses a flow
 * analysis with the tier off, because it would produce an empty run on every fire. Offering it and
 * then refusing the save teaches nothing; not offering it is the honest shape.
 */
export function schedulableTools(flowEnabled: boolean): typeof TOOLS {
  return flowEnabled ? TOOLS : TOOLS.filter((t) => t.method !== 'flow');
}

/** `HH:MM` for a schedule's firing time, zero-padded. */
export function timeLabel(atHour: number, atMinute: number): string {
  return `${String(atHour).padStart(2, '0')}:${String(atMinute).padStart(2, '0')}`;
}
