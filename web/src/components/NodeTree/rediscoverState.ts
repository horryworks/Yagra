// SPDX-License-Identifier: AGPL-3.0-only
// What the Rediscover dialog shows and what its Apply sends (ADR-186).
//
// Pure, so the node-environment Vitest reaches it: the dialog in `RediscoverModal.tsx` is the
// wiring. The one rule that matters most lives here — a re-read that has not answered yet is
// never "nothing changed". Waiting and silence are phases of their own, and only `answered`
// carries rows at all.
import type {
  RediscoverApplyBody,
  RediscoverComparison,
  RediscoverVerdict,
  RediscoverView,
} from '../../types/api';

/** How long the dialog waits for any poller to pick the re-read up before saying so. */
export const PICKUP_WARN_MS = 60_000;

/** How often the dialog asks again while the re-read is in flight. */
export const REDISCOVER_POLL_MS = 2_000;

/** The three rows, in the order the dialog lists them. */
export const REDISCOVER_FIELDS = ['profile', 'vendor', 'model'] as const;
export type RediscoverField = (typeof REDISCOVER_FIELDS)[number];

export type Phase =
  /** The start request has not come back. */
  | 'starting'
  /** Published; no poller has said anything yet. */
  | 'waiting'
  /** …and it has been long enough that the operator should know. */
  | 'waitingLong'
  /** A poller is asking the device. */
  | 'reading'
  | 'answered'
  | 'noSnmpAnswer'
  | 'noAnswer'
  | 'stopped'
  /** The scan is gone from the core (restart, eviction, a standby answered). */
  | 'lost';

/** Which phase to show, from the last view read and how long ago the start was accepted. */
export function phaseOf(view: RediscoverView | null, lost: boolean, elapsedMs: number): Phase {
  if (lost) return 'lost';
  if (!view) return 'starting';
  switch (view.state) {
    case 'waiting':
      return elapsedMs >= PICKUP_WARN_MS ? 'waitingLong' : 'waiting';
    case 'reading':
      return 'reading';
    case 'answered':
      // An `answered` with no comparison would be a server defect; showing rows that are not there
      // is worse than showing it as still reading, which at least keeps asking.
      return view.comparison ? 'answered' : 'reading';
    case 'no_snmp_answer':
      return 'noSnmpAnswer';
    case 'no_answer':
      return 'noAnswer';
    case 'stopped':
      return 'stopped';
    default: {
      const unknown: never = view.state;
      void unknown;
      // A state a newer core sends: keep asking rather than claim an outcome.
      return 'reading';
    }
  }
}

/** Whether the dialog should keep asking. */
export function keepPolling(phase: Phase): boolean {
  switch (phase) {
    case 'starting':
    case 'waiting':
    case 'waitingLong':
    case 'reading':
      return true;
    case 'answered':
    case 'noSnmpAnswer':
    case 'noAnswer':
    case 'stopped':
    case 'lost':
      return false;
    default: {
      const unknown: never = phase;
      void unknown;
      return false;
    }
  }
}

/** One row as the dialog draws it. */
export interface RowView {
  field: RediscoverField;
  current: string | null;
  found: string | null;
  verdict: RediscoverVerdict;
}

/** The three rows, with profile ids turned into names (an id is never shown). */
export function rowsOf(c: RediscoverComparison): RowView[] {
  return [
    {
      field: 'profile',
      current: c.profile.current_name ?? null,
      found: c.profile.found_name ?? c.profile.found_id ?? null,
      verdict: c.profile.verdict,
    },
    {
      field: 'vendor',
      current: c.vendor.current ?? null,
      found: c.vendor.found ?? null,
      verdict: c.vendor.verdict,
    },
    {
      field: 'model',
      current: c.model.current ?? null,
      found: c.model.found ?? null,
      verdict: c.model.verdict,
    },
  ];
}

/** The rows Apply may write — only those that differ. */
export function applicable(c: RediscoverComparison): RediscoverField[] {
  return rowsOf(c)
    .filter((r) => r.verdict === 'differs')
    .map((r) => r.field);
}

/** The body for the chosen rows, each echoing what the dialog showed as current. `null` when
 *  nothing chosen can be applied. */
export function applyBody(
  scanId: string,
  c: RediscoverComparison,
  chosen: ReadonlySet<RediscoverField>,
): RediscoverApplyBody | null {
  const can = new Set(applicable(c));
  const body: RediscoverApplyBody = { scan_id: scanId };
  if (chosen.has('profile') && can.has('profile') && c.profile.found_id) {
    body.profile = { from: c.profile.current_id ?? null, to: c.profile.found_id };
  }
  if (chosen.has('vendor') && can.has('vendor') && c.vendor.found) {
    body.vendor = { from: c.vendor.current ?? null, to: c.vendor.found };
  }
  if (chosen.has('model') && can.has('model') && c.model.found) {
    body.model = { from: c.model.current ?? null, to: c.model.found };
  }
  return body.profile || body.vendor || body.model ? body : null;
}

/** The dialog's own words for a refusal it can explain better than the server's English. `null`
 *  falls through to the server's message. */
export function refusalKey(code: string | undefined): string | null {
  switch (code) {
    case 'no_live_poller':
      return 'rediscover.err.noLivePoller';
    case 'no_snmp_credential':
      return 'rediscover.err.noCredential';
    case 'not_a_device':
      return 'rediscover.err.notADevice';
    case 'profile_locked':
      return 'rediscover.err.locked';
    case 'node_changed':
    case 'judgement_changed':
      return 'rediscover.err.changed';
    case 'scan_not_found':
      return 'rediscover.err.lost';
    default:
      return null;
  }
}

/** Whether an Apply refusal means the comparison on screen is stale: the node or the rules moved
 *  after the device was read. The server rebuilds the comparison on every read, so one more GET
 *  shows the current values against the same answer — no new scan (ADR-186 増分 2). Sending the
 *  same stale body again would only earn the same refusal. */
export function rereadOn(code: string | undefined): boolean {
  return code === 'node_changed' || code === 'judgement_changed';
}
