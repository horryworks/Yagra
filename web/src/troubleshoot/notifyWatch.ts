// SPDX-License-Identifier: AGPL-3.0-only
// "Notify me" for an analysis run.
//
// The launch drawer has always offered a Notify-me choice and the backend has always persisted it
// (`AnalysisJob.params.notify`), but nothing ever consumed it: the operator picked "notify me" and
// nothing arrived, with no text anywhere saying so.
//
// This delivers it as an in-app completion notice. Deliberately *not* through the alert
// dispatcher: that path is keyed by `DedupKey { node, check, severity }` (an all-scope analysis has
// no node), keeps a permanent entry per dispatch, and routes by severity to PagerDuty/JSM — so
// "your capacity report finished" would page an on-call engineer, polluting exactly the clean
// alert-lifecycle signal ADR-015 exists to protect. There is also no per-user address anywhere in
// `users` to send "me" anything.
//
// The honest limitation — it fires only while Yagra is open in this browser — is stated in the
// drawer hint rather than left for the operator to discover.

import type { AnalysisJob } from '../types/api';
import { reportPathFor, toolById } from './data';

/** Terminal for notification purposes: anything that is not still in flight.
 *
 *  Broader than `TERMINAL_JOB_STATES` in `data.ts`, which is the subset whose *label* is shown and
 *  therefore needs locale strings. A successful run is terminal too, and is the case an operator
 *  most wants to hear about. */
export function isFinished(state: string): boolean {
  return state !== 'running' && state !== 'queued';
}

/** What the completion notice should say and where it links. */
export interface NotifyPlan {
  /** i18n key under `troubleshoot:`. */
  msgKey: 'toast.finished' | 'toast.failed';
  /** In-app route for the toast's "View →". */
  linkTo: string;
}

/** Whether this job update is the moment to notify, and with what.
 *
 *  Fires on the *transition* into a finished state, so a re-render or a repeated SSE tick carrying
 *  the same terminal row does not notify twice. A job that is already finished the first time it
 *  is seen (a late subscribe, or a page load after the fact) does not notify either — the operator
 *  did not watch it finish, so announcing it would be noise. */
export function shouldNotify(
  prev: AnalysisJob | undefined,
  next: AnalysisJob,
  watched: ReadonlySet<string>,
): NotifyPlan | null {
  if (!watched.has(next.id)) return null;
  if (!prev) return null; // first sighting — the operator did not watch this one finish
  if (isFinished(prev.state)) return null; // already finished; this is a repeat tick
  if (!isFinished(next.state)) return null; // still in flight
  return {
    msgKey: next.state === 'succeeded' ? 'toast.finished' : 'toast.failed',
    linkTo: reportLinkFor(next),
  };
}

/** The report route for a job, matching how the runs list and the launch drawer build it.
 *
 *  A job naming a tool this build does not know (a newer core) has no report to open, so the link
 *  goes to the catalog — which is where an unknown report path redirects anyway. */
export function reportLinkFor(job: AnalysisJob): string {
  const tool = toolById(job.tool);
  if (!tool) return '/troubleshoot';
  return `${reportPathFor(tool.id)}?job=${encodeURIComponent(job.id)}`;
}
