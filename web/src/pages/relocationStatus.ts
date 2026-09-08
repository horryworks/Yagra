// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind Settings ▸ Move to another server (ADR-121), kept out of the .tsx so it can
// be tested. (Vitest runs `src/**/*.test.ts` in a node environment — a test written in a .tsx file
// is a file nothing runs. See .claude/rules/testing.md.)
//
// It is the sibling of `upgradeStatus.ts` and follows the same three rules:
//   * every union that anything iterates is an `as const` array, so `i18nEnumKeys.test.ts` can
//     demand its strings in both locales (extensibility.md §4);
//   * every token that arrives as a plain string is read **defensively**, because a newer sidecar
//     may write one this build has never heard of and that must render as *something*;
//   * nothing here re-derives a decision the backend already made.

import type { RelocationStatus } from '../types/api';

/** What a request asks for. Mirrors `RelocationMode` in `crates/yagra-core/src/relocation.rs`. */
export const RELOCATION_MODES = ['archive', 'preflight', 'push'] as const;
export type RelocationMode = (typeof RELOCATION_MODES)[number];

/** How to authenticate to the target host. */
export const RELOCATION_AUTH_KINDS = ['password', 'key'] as const;
export type RelocationAuthKind = (typeof RELOCATION_AUTH_KINDS)[number];

/** The run states the sidecar writes. `requested` and `running` both mean "still going" — the
 *  first is the arm's own first write, before the container has started. */
export const RELOCATION_STATES = ['requested', 'running', 'done', 'failed'] as const;
export type RelocationState = (typeof RELOCATION_STATES)[number];

/** The stages the procedure walks, in the order it walks them.
 *
 *  `validate` is not one of them — it is what a *refused* request is stamped with, so it has a
 *  label but no place on the track. Same separation `upgradeStatus.ts` keeps, and for the same
 *  reason: a refusal must not render as "20% done". */
export const RELOCATION_PROGRESS_STAGES = [
  'start',
  'preflight',
  'docker',
  'backup',
  'files',
  'images',
  'tier2',
  'archive',
  'push',
] as const;

/** Every stage that can reach the page: the track plus the refusal stamp. */
export const RELOCATION_STAGES = [...RELOCATION_PROGRESS_STAGES, 'validate'] as const;
export type RelocationStage = (typeof RELOCATION_STAGES)[number];

/** Why the page cannot offer to move anything. */
export const RELOCATION_READINESS = [
  'unsupported',
  'absent',
  'stopped',
  'paused',
  'updater_too_old',
  'ready',
] as const;
export type Readiness = (typeof RELOCATION_READINESS)[number];

/**
 * Six states, narrowing in the order an operator acts on them.
 *
 * The first four are `upgradeStatus.ts::mechanism`'s, because it is the *same sidecar* and the
 * same switch — turning upgrades off turns this off too, deliberately. The fifth is this feature's
 * own: a deployment installed before ADR-121 has a perfectly healthy updater that does not know
 * the command, and the fix is an upgrade rather than anything on this page.
 *
 * ⚠️ `updater_too_old` is checked **last**, after liveness. A sidecar that is not running cannot
 * declare a capability either, so asking about the capability first would report every dead
 * updater as an out-of-date one and send the operator to the wrong page.
 */
export function readiness(status: RelocationStatus): Readiness {
  if (!status.updater.installed) return 'unsupported';
  if (!status.updater.present) return 'absent';
  if (!status.updater.fresh) return 'stopped';
  if (!status.upgrade_enabled) return 'paused';
  if (!status.supported) return 'updater_too_old';
  return 'ready';
}

/** Read the state defensively. */
export function relocationState(raw: string | null | undefined): RelocationState | null {
  return RELOCATION_STATES.includes(raw as RelocationState) ? (raw as RelocationState) : null;
}

/** Read the stage defensively. */
export function relocationStage(raw: string | null | undefined): RelocationStage | null {
  return RELOCATION_STAGES.includes(raw as RelocationStage) ? (raw as RelocationStage) : null;
}

export type RelocationRun = NonNullable<RelocationStatus['run']>;

/**
 * What the page should be showing, given what the server said and what this browser asked for.
 *
 * `pending` is the run id `POST` returned. It exists for the same window `upgradeStatus.ts`
 * describes: the sidecar looks for the request every five seconds and then has to start a
 * container, so for 5–10 seconds `relocation.json` still describes the *previous* run. Reading the
 * server alone in that window shows the operator someone else's outcome as the answer to the
 * button they just pressed.
 *
 * ⚠️ It never decides the outcome — it only separates "asked, not visible yet" from "idle", and it
 * is matched by id, so a stale `pending` cannot claim an unrelated run.
 */
export type RunPhase =
  | { kind: 'idle' }
  | { kind: 'starting' }
  | { kind: 'running'; stage: RelocationStage | null; index: number; total: number }
  | { kind: 'done'; state: RelocationState; run: RelocationRun };

export function runPhase(status: RelocationStatus, pending: string | null): RunPhase {
  const run = status.run ?? null;
  const state = relocationState(run?.state);
  if (run && (state === 'requested' || state === 'running')) {
    const stage = relocationStage(run.stage);
    const index = RELOCATION_PROGRESS_STAGES.findIndex((s) => s === stage);
    return { kind: 'running', stage, index, total: RELOCATION_PROGRESS_STAGES.length };
  }
  if (run && pending !== null && run.id === pending) {
    return { kind: 'done', state: state ?? 'failed', run };
  }
  if (pending !== null) return { kind: 'starting' };
  // No pending run of this browser's, so the last one the server holds is history rather than an
  // answer — but it is still worth showing, which is why `done` is reachable without `pending`.
  if (run && (state === 'done' || state === 'failed')) {
    return { kind: 'done', state, run };
  }
  return { kind: 'idle' };
}

/** Should the page keep re-reading?
 *
 *  Armed by the act of starting, not by what the server currently says — the inversion that was
 *  the bug on the Upgrade page. */
export function shouldPoll(status: RelocationStatus, pending: string | null): boolean {
  const phase = runPhase(status, pending);
  return phase.kind === 'starting' || phase.kind === 'running';
}

/** How far along the track, 0…1, or `null` when there is no determinate position.
 *
 *  `null` rather than 0 for an unrecognised stage: a bar pinned at the left claims nothing has
 *  happened, and "this build does not know this stage" is not that claim.
 *
 *  ⚠️ A *skipped* stage still counts. `images` and `tier2` are optional, so a run that skips both
 *  jumps from `files` to `archive` — the bar moves in bigger steps, which is honest, rather than
 *  the track shrinking under it while it is being watched. */
export function stageProgress(phase: RunPhase): number | null {
  if (phase.kind === 'done') return 1;
  if (phase.kind !== 'running' || phase.index < 0) return null;
  return (phase.index + 1) / (phase.total + 1);
}

/** May a relocation be started right now?
 *
 *  `enabled` is the backend's own answer to "would a request be accepted", so this does not
 *  re-derive it — it only adds the window in which the backend's 409 cannot see the run yet,
 *  exactly as `canApply` does on the Upgrade page. */
export function canStart(status: RelocationStatus, pending: string | null = null): boolean {
  return status.enabled && !shouldPoll(status, pending);
}

/** The three commands an operator types on the new server, having carried the archive there.
 *
 *  ⚠️ Kept in step with `scripts/RELOCATION-README.md`, which is shown *inside* the archive. Two
 *  copies of one procedure; this one is the one an operator reads before downloading, so it must
 *  not be the one that drifts. */
export function targetCommands(filename: string): string[] {
  return ['mkdir yagra && cd yagra', `tar -xzf ~/${filename}`, './yagra-relocate.sh'];
}

/** What the operator typed into the target form. Strings, because that is what an input holds. */
export interface TargetForm {
  host: string;
  port: string;
  user: string;
  dir: string;
  kind: RelocationAuthKind;
  secret: string;
}

/**
 * i18n keys for everything wrong with the form, in field order.
 *
 * **Deliberately the same charsets as the backend**, which is a duplication with a reason: this
 * side gives the operator a message beside the field, the backend's is the one that must be true,
 * and the sidecar re-checks a third time because it reads the request file as root. Neither of the
 * first two is a substitute for the third.
 *
 * Returns keys rather than sentences so both locales are covered by `i18nEnumKeys.test.ts`.
 */
export function validateTarget(f: TargetForm): string[] {
  const out: string[] = [];
  const host = f.host.trim();
  if (host === '' || host.length > 253 || !/^[A-Za-z0-9.:-]+$/.test(host)) out.push('badHost');
  const port = Number(f.port);
  if (!/^\d{1,5}$/.test(f.port.trim()) || port < 1 || port > 65535) out.push('badPort');
  if (!/^[a-z_][a-z0-9_-]{0,31}$/.test(f.user.trim())) out.push('badUser');
  if (!/^[A-Za-z0-9._-]{1,64}$/.test(f.dir.trim())) out.push('badDir');
  if (f.secret === '') out.push('noSecret');
  return out;
}

/** Bytes → "1.2 GB", for the three size numbers the status carries. Uses the shared formatter at
 *  the call site; this only exists to say `null` when the server could not measure. */
export function knownBytes(n: number | null | undefined): number | null {
  return typeof n === 'number' && Number.isFinite(n) && n >= 0 ? n : null;
}

/** Is there enough room, as far as core can tell?
 *
 *  ⚠️ **`true` is not a promise.** The estimate omits the event and flow stores and the image
 *  archive, which core cannot measure — only the sidecar can, and its check is the one that can
 *  stop a run. `null` means the question could not be asked, which the page must render as
 *  "unknown" rather than as either answer. */
export function roomForArchive(status: RelocationStatus): boolean | null {
  const free = knownBytes(status.free_bytes);
  const needed = knownBytes(status.needed_bytes);
  if (free === null || needed === null) return null;
  return free >= needed;
}
