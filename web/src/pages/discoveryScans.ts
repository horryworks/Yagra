// SPDX-License-Identifier: AGPL-3.0-only
// Judgement for the Discovery screen's scan list (ADR-068 Increment 1), kept out of the `.tsx` so
// it can be tested: Vitest runs with `environment: 'node'` and `include: ['src/**/*.test.ts']`, so
// a test written beside the component would never execute.
//
// What lives here is everything that decides *what the page shows*: which scan to reattach to on
// arrival, whether to keep polling, how to render a state, and which pool to sweep from. What stays
// in the component is layout and effects.

import type { DiscoveryScan, DiscoveryScanSummary, PoolOption } from '../types/api';
import { DISCOVERY_SCAN_STATES, type DiscoveryScanState } from '../types/api';

/** How a scan state is presented. `unknown` is this side's invention — the wire has no such value.
 *
 *  ⚠️ Tone matters here in a way that has already cost something. A `switch` with a `default:` arm
 *  painted unrecognised report-run states red, so a state this build had merely never been taught
 *  read as a failure. An exhaustive `Record` plus a neutral fallback is the fix, and the fallback
 *  must stay neutral: not knowing the word is not the same as bad news. */
export interface ScanStateSpec {
  tone: 'info' | 'up' | 'critical' | 'neutral';
  labelKey: string;
  /** Whether more results may still arrive — i.e. whether the page should keep polling. */
  inFlight: boolean;
}

/** The label/tone table. Keyed by the union, so a new backend state is a compile error here. */
export const SCAN_STATE_SPECS: Record<DiscoveryScanState | 'unknown', ScanStateSpec> = {
  running: { tone: 'info', labelKey: 'discovery.scans.state.running', inFlight: true },
  // Still in flight: the poller may yet report, and that report is what turns "we asked it to
  // stop" into "it stopped". Polling through this state is how the page learns which happened.
  cancelling: { tone: 'info', labelKey: 'discovery.scans.state.cancelling', inFlight: true },
  cancelled: { tone: 'neutral', labelKey: 'discovery.scans.state.cancelled', inFlight: false },
  done: { tone: 'up', labelKey: 'discovery.scans.state.done', inFlight: false },
  // Neutral, never critical: the sweep may well be fine — this build just does not know the word.
  //
  // ⚠️ But **not in flight**, and the two halves of that are decided by different arguments. The
  // tone is about what to tell the operator (nothing bad is known). `inFlight` is about whether to
  // keep asking, and there the answer must be no: a build that cannot read a state today will not
  // learn it tomorrow, so treating unknown as "still going" is an unbounded poll rather than
  // caution. The backend reaches the same conclusion for the same reason — see
  // `AnalysisJobState::is_terminal`, where `Unknown` counts as terminal.
  unknown: { tone: 'neutral', labelKey: 'discovery.scans.state.unknown', inFlight: false },
};

/** Narrow a wire value to a state this build can render. */
export function scanState(raw: string | null | undefined): DiscoveryScanState | 'unknown' {
  return (DISCOVERY_SCAN_STATES as readonly string[]).includes(raw ?? '')
    ? (raw as DiscoveryScanState)
    : 'unknown';
}

/** Whether a scan may still produce results. */
export function isScanInFlight(state: string | null | undefined): boolean {
  return SCAN_STATE_SPECS[scanState(state)].inFlight;
}

/** Which scan the page should open when the operator arrives.
 *
 *  Precedence, and each step earns its place:
 *  1. **the id in the URL** — an explicit request (a reload, a bookmark, a shared link) outranks
 *     any guess. Returned even when the list does not contain it, so the caller can fetch it and
 *     say "this core does not know that scan" rather than silently opening something else;
 *  2. **the newest in-flight scan** — the reason this feature exists;
 *  3. **the newest scan of any kind** — coming back to finished candidates is the other half of
 *     "I left the page and came back".
 *
 *  `null` only when there is genuinely nothing to show. */
export function selectInitialScan(
  scans: readonly DiscoveryScanSummary[],
  urlScanId: string | null,
): string | null {
  if (urlScanId) return urlScanId;
  // The list arrives newest-first from the server; `find` therefore picks the newest match without
  // re-sorting. (Re-sorting here would be a second copy of an ordering the server already owns.)
  const running = scans.find((s) => isScanInFlight(s.state));
  return running?.scan_id ?? scans[0]?.scan_id ?? null;
}

/** Inputs to the polling decision, named so the call site cannot transpose two booleans. */
export interface PollInputs {
  /** The scan the page is showing. */
  scanId: string | null;
  /** The last status fetched, or `null` when none has been. ⚠️ Not necessarily about `scanId` —
   *  see the identity check in `shouldPollScan`. */
  status: DiscoveryScan | null;
  /** The operator pressed Scan in this session and the server has not answered yet. */
  justStarted: boolean;
  /** Consecutive failed status fetches. */
  failures: number;
}

/** After this many consecutive failures the page stops asking.
 *
 *  Before ADR-068 there was no such stop: a scan the core had forgotten left the page hammering a
 *  404 every two seconds forever, with the progress note frozen at whatever it last said. */
export const MAX_POLL_FAILURES = 5;

/** Should the page keep re-reading the scan?
 *
 *  Driven by **the act of starting** and by *not having been told it is over* — never by the
 *  server having said "running". That inversion is a bug this repo has already paid for once
 *  (`upgradeStatus.ts::shouldPoll` names it): while the server cannot yet describe the work,
 *  reading its answer literally means concluding there is nothing to watch, and the page goes
 *  silent exactly when it should be live.
 *
 *  So the rule is negative: poll unless something says stop. A terminal state stops it; repeated
 *  failure stops it; nothing selected stops it; and a state this build cannot read stops it too —
 *  see `SCAN_STATE_SPECS.unknown`, where waiting for a word that will never arrive is the worse of
 *  the two mistakes. */
export function shouldPollScan({ scanId, status, justStarted, failures }: PollInputs): boolean {
  if (failures >= MAX_POLL_FAILURES) return false;
  if (justStarted) return true;
  // 🚨 `null` means "we have not asked yet", **not** "there is nothing to watch". Reading it as the
  // latter is the exact inversion this function's doc warns about, and it shipped anyway: the
  // reattach path selects a scan id from the list and has never fetched it, so it arrives here with
  // `justStarted: false` and `status: null` — and a running sweep was never polled at all. On a
  // real deployment that looked like the whole feature half-working: the sweep listed as `Running`
  // in Recent sweeps, while the progress line, the Stop button and the disabled Scan button (all
  // derived from `status`) were simply absent.
  //
  // Whether a scan is *selected* is the caller's question, not this one's — the effect that polls
  // is already guarded on having a scan id.
  if (!status) return true;
  // 🚨 …and neither does a status about a **different** scan, which is the same mistake one
  // level subtler. Pressing Scan while a finished sweep is on screen starts a poll immediately (on
  // `justStarted`), and the id it polls is still the *old* one, because the new id only exists once
  // the server answers. That old poll lands first, clears `justStarted` and installs a `done`
  // status — so by the time the new scan id arrives, this function has already concluded there is
  // nothing to watch. The sweep then ran to completion with the row frozen at `0/254` and the
  // progress line stuck, and only a reload showed it had finished.
  //
  // Reattach made this the *normal* path rather than an edge case: arriving at the page now selects
  // the newest sweep, so there is almost always a finished scan on screen when Scan is pressed.
  if (scanId && status.scan_id !== scanId) return true;
  return isScanInFlight(status.state);
}

/** Fold a freshly-polled scan back into the list of sweeps.
 *
 *  🚨 **The list is fetched, the selected scan is polled, and only the second of those repeats.**
 *  So the row in *Recent sweeps* froze at whatever the list said when it was last fetched: a sweep
 *  the operator was watching sat at `Running · 0/254 probed · 0 devices` while the progress line and
 *  the candidate table directly below it filled in. Two surfaces showing one fact, fetched
 *  separately, and the one that updated less often was the one wearing the word "Running".
 *
 *  Refreshing the list on every tick would fix the symptom and keep the cause — two fetches that can
 *  disagree, at twice the request rate. Deriving the row from the status the page already has cannot
 *  disagree, because there is only one answer.
 *
 *  A scan with no row yet is **prepended**: the list arrives newest-first, and the only way to be
 *  polling a scan the list has never mentioned is to have just started it. */
export function mergeScanIntoList(
  scans: readonly DiscoveryScanSummary[],
  status: DiscoveryScan,
): DiscoveryScanSummary[] {
  const row: DiscoveryScanSummary = {
    scan_id: status.scan_id,
    state: status.state,
    probed: status.probed,
    total: status.total,
    candidate_count: status.candidates.length,
    started_at: status.started_at,
    updated_at: status.updated_at,
    pool: status.pool,
  };
  const at = scans.findIndex((s) => s.scan_id === status.scan_id);
  if (at < 0) return [row, ...scans];
  // Rebuilt in place: the list's order is the server's (newest first) and moving a row because it
  // reported progress would shuffle the list under the operator's cursor.
  return scans.map((s, i) => (i === at ? row : s));
}

/** The pool a fresh visit should preselect.
 *
 *  One live pool ⇒ that one: a single-site deployment should not have to answer a question with
 *  one possible answer. Several ⇒ `default` if it is live, otherwise nothing, because guessing
 *  which *site* to sweep from is exactly the guess that made this worth fixing.
 *
 *  ⚠️ Returns `null` rather than a dead pool. Selecting one would send the sweep to a route with
 *  no listener, which the server then quietly redirects to the global subject — the very behaviour
 *  this control exists to make visible. */
export function pickDefaultPool(pools: readonly PoolOption[]): string | null {
  const live = pools.filter((p) => p.live);
  if (live.length === 1) return live[0].name;
  return live.find((p) => p.name === 'default')?.name ?? null;
}

/** Whether the operator may ask this sweep to stop.
 *
 *  Only a sweep still running: one already being stopped has been asked, and a terminal one has
 *  nothing left to stop. ⚠️ The control is **removed** in those cases rather than disabled — a
 *  disabled button explains itself only on hover, which is nothing at all on a touch device
 *  (`ui-conventions.md`). What replaces it is a line of text saying the stop was requested.
 *
 *  Takes the raw wire value so an unrecognised state answers `false`: offering to stop a sweep this
 *  build cannot reason about would be guessing at its behalf. */
export function canRequestStop(state: string | null | undefined): boolean {
  return scanState(state) === 'running';
}

/** Whether choosing this pool means the sweep's site is undecided.
 *
 *  True for "no pool chosen" and for a pool with no live poller — the server treats both the same
 *  way (global subject, first poller to answer wins), so the screen must too. Two visibly
 *  different choices with one silent outcome is how the original defect read as normal.
 *
 *  ⚠️ This is a warning, not a veto: a pool that is briefly down is still the pool the operator
 *  means, and removing it from the list would take away the reason to come back to it. */
export function poolIsUnrouted(pools: readonly PoolOption[], chosen: string | null): boolean {
  if (!chosen) return true;
  return !pools.some((p) => p.name === chosen && p.live);
}
