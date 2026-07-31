// SPDX-License-Identifier: AGPL-3.0-only
// How a report run's state and a schedule's cadence are presented — as maps keyed by the union, not
// as switch statements.
//
// The switch this replaces ended in `default: <Badge tone="critical">Failed</Badge>`. Because the
// backend typed `state` as a bare `String`, TypeScript could not object, so any state the UI had
// not been taught about was shown to the operator as a red **failure**. That is the worst available
// reading of "we don't recognise this": a run that succeeded on a newer core would appear to have
// broken. Now that `ReportRunState` is a real union (ADR-035), a `Record` keyed by it makes a new
// variant a compile error at the one place that has to decide what it looks like.
//
// Pure and in a `.ts` on purpose: Vitest only runs `src/**/*.test.ts`, so judgement that lives in a
// `.tsx` is judgement no test can reach (see .claude/rules/testing.md).

import type { Tone } from '../components/ui/Badge';
import {
  REPORT_FREQUENCIES,
  REPORT_RUN_STATES,
  type ReportFrequency,
  type ReportRunState,
} from '../types/api';

export interface RunStatusSpec {
  tone: Tone;
  /** Key in the `reports` namespace. */
  labelKey: string;
  /** Whether the label interpolates `{{pct}}` — only the live state does. */
  showsPct?: boolean;
  /** Still generating: the viewer polls while this holds, and the list counts them. */
  inFlight?: boolean;
}

/** One entry per run state. A new state added to the backend fails to compile here. */
export const RUN_STATUS: Record<ReportRunState, RunStatusSpec> = {
  queued: { tone: 'info', labelKey: 'run.status.queued', inFlight: true },
  running: { tone: 'info', labelKey: 'run.status.generating', showsPct: true, inFlight: true },
  succeeded: { tone: 'up', labelKey: 'run.status.ready' },
  failed: { tone: 'critical', labelKey: 'run.status.failed' },
  // Neutral, not critical: the run may well have succeeded — this core just does not know the word.
  unknown: { tone: 'neutral', labelKey: 'run.status.unknown' },
};

/** Whether a run is still being generated. Replaces three hand-written copies of
 *  `state === 'running' || state === 'queued'`, which is the sort of pair that drifts when a third
 *  in-flight state appears. */
export function isRunInFlight(state: ReportRunState): boolean {
  return RUN_STATUS[state].inFlight === true;
}

/** Narrow an untrusted string (an SSE payload) to a run state. */
export function isRunState(v: unknown): v is ReportRunState {
  return typeof v === 'string' && (REPORT_RUN_STATES as readonly string[]).includes(v);
}

/** Cadence label key + which extra field it interpolates. Same shape, same reason: `cadenceLabel`
 *  had the identical `default:` bug and rendered an unknown cadence as "Daily". */
export const CADENCE: Record<ReportFrequency, { labelKey: string; part: 'day' | 'dom' | 'none' }> = {
  daily: { labelKey: 'reports:cadence.daily', part: 'none' },
  weekly: { labelKey: 'reports:cadence.weekly', part: 'day' },
  monthly: { labelKey: 'reports:cadence.monthly', part: 'dom' },
  unknown: { labelKey: 'reports:cadence.unknown', part: 'none' },
};

/** The cadences an operator may actually choose — `unknown` is a storage degradation, never a
 *  selectable option. A deliberate subset of the union, pinned by a test (the `monitorKinds.ts`
 *  pattern) so a fourth cadence forces the question rather than being silently excluded. */
export const SELECTABLE_FREQUENCIES = REPORT_FREQUENCIES.filter(
  (f) => f !== 'unknown',
) as readonly Exclude<ReportFrequency, 'unknown'>[];
