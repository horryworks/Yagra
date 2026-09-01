// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind Settings ▸ Upgrade, kept out of the .tsx so it can be tested.
// (Vitest runs `src/**/*.test.ts` in a node environment — a test written in a .tsx file is a file
// nothing runs. See .claude/rules/testing.md.)
//
// Deliberately does NO version arithmetic. The compatibility floor is computed in Rust
// (`crates/yagra-core/src/upgrade.rs::binding_floor`), where `semver` orders 0.2.10 after 0.2.9 and
// a test pins that. Re-deriving it here would put the one rule that decides whether a rollback is
// offered in two places, in two languages — exactly the drift `.claude/rules/extensibility.md` §2
// names. This module only decides what the page is allowed to *say*.

import type { UpgradeStatus } from '../types/api';

/** What state the privileged updater sidecar is in (ADR-050 decision 1). */
export const MECHANISMS = ['unsupported', 'absent', 'stopped', 'paused', 'ready'] as const;
export type Mechanism = (typeof MECHANISMS)[number];

/**
 * The i18n key stem each state renders under, in the `settings-upgrade` namespace.
 *
 * A `Record`, so adding a state to `MECHANISMS` is a compile error until it has somewhere to say
 * itself, and `i18nEnumKeys.test.ts` iterates this to demand the strings exist in both locales.
 * Neither guard existed while the union was a bare one: a fifth state could ship rendering nothing
 * at all, in either language, with every test green.
 *
 * It is a map rather than a naming convention because the stems do not match the state names —
 * `absent` renders under `mechanism.disabled*`, which predates the states being named at all.
 */
export const MECHANISM_KEYS: Record<Mechanism, string> = {
  unsupported: 'unsupported',
  absent: 'disabled',
  stopped: 'stopped',
  paused: 'paused',
  // `ready` has no headline sentence — the release picker is the message.
  ready: 'readyFrom',
};

/**
 * Five states, and every boundary between them earns its place.
 *
 * `unsupported` — this deployment has no upgrade mechanism at all, because of how it was installed:
 *   the apply half needs a container composition carrying the privileged updater alongside core,
 *   which a native install (and a composition that does not ship it) has no way to provide. Not a
 *   fault, and nothing on this host will fix it.
 * `absent` — there *is* a mechanism and no sidecar has ever reported. Since it ships in the
 *   composition, this means it was removed, or the stack has not finished starting.
 * `stopped` — it reported once and has gone quiet. A fault; the fix is to read its logs.
 * `paused` — alive, and the operator switched the mechanism off. A choice, not a fault.
 * `ready` — alive and permitted.
 *
 * Rendering any two of these the same would train an operator to ignore the one that matters. The
 * first boundary is the one worth guarding hardest: reading `unsupported` off `present` instead of
 * off `installed` would make a *dead sidecar* vanish from the page that exists to report on it —
 * the page would go quiet exactly when something broke. All five are also distinct from "there is
 * no newer version", which is a further fact entirely — same discipline ADR-040 applied to
 * `/flags`: never present an inferred value as a known one.
 *
 * Reads `upgrade_enabled` (what this deployment stored) rather than `updater.paused` (what the
 * sidecar last saw). They converge within one beat, and the stored value is the one the toggle
 * shows, so following it keeps the switch from appearing to bounce back after a click.
 */
export function mechanism(status: UpgradeStatus): Mechanism {
  if (!status.updater.installed) return 'unsupported';
  if (!status.updater.present) return 'absent';
  if (!status.updater.fresh) return 'stopped';
  return status.upgrade_enabled ? 'ready' : 'paused';
}

/** Has the operator's switch reached the sidecar yet? Drives the "saved, applying…" hint — the two
 *  disagree for at most one of the sidecar's beats. */
export function switchPending(status: UpgradeStatus): boolean {
  return status.updater.present && status.updater.paused === status.upgrade_enabled;
}

/** The run states the updater writes. Iterated by `i18nEnumKeys.test.ts`, which is what stops a
 *  new one shipping with no strings in either locale. */
export const UPGRADE_RUN_STATES = ['running', 'succeeded', 'failed', 'rejected'] as const;
export type UpgradeRunState = (typeof UPGRADE_RUN_STATES)[number];

/** Read the run state defensively: it arrives as a plain string, so an unrecognised value from a
 *  newer updater must render as *something* rather than as a missing translation key. */
export function runState(raw: string | undefined | null): UpgradeRunState | null {
  return UPGRADE_RUN_STATES.includes(raw as UpgradeRunState) ? (raw as UpgradeRunState) : null;
}

/** Is an upgrade happening right now? Drives the "your session will drop" waiting state. */
export function isRunning(status: UpgradeStatus): boolean {
  return status.last_run?.state === 'running';
}

/** The phases the sidecar writes into `status.json`, in the order it writes them.
 *
 *  `validate` is not one of them — it is what a *refused* request is stamped with, so it has a
 *  label but no place on the track. Keeping the two lists separate is what stops a rejection
 *  rendering as "20% done". */
export const UPGRADE_PROGRESS_STEPS = ['start', 'backup', 'pull', 'compose', 'verify'] as const;

/** Every step that can reach the page, which is the track plus the refusal stamp. Iterated by
 *  `i18nEnumKeys.test.ts` — before this existed the page printed the raw shell token. */
export const UPGRADE_RUN_STEPS = [...UPGRADE_PROGRESS_STEPS, 'validate'] as const;
export type UpgradeRunStep = (typeof UPGRADE_RUN_STEPS)[number];

/** Read the step defensively, exactly as `runState` reads the state: a newer sidecar may write one
 *  this build has never heard of, and that must render as *something*. */
export function runStep(raw: string | undefined | null): UpgradeRunStep | null {
  return UPGRADE_RUN_STEPS.includes(raw as UpgradeRunStep) ? (raw as UpgradeRunStep) : null;
}

/**
 * What the page should be showing about the run, given what the server said and what this browser
 * asked for.
 *
 * `pending` is the run id `POST` returned, or `null` when this browser has not started one. It
 * exists because **there is a window in which the server can say nothing at all**: core hands the
 * request to the sidecar, which only looks for it every five seconds and then has to start a
 * container, so `status.json` does not mention the new run for the first 5–10 seconds. Reading the
 * server alone during that window returns the *previous, finished* run — which is what made the
 * page sit silent through an entire upgrade (it armed its poll from `isRunning`, which was false).
 *
 * ⚠️ `pending` never decides the **outcome** (ADR-050 decision 3: the browser must not hold it).
 * It only distinguishes "asked, not visible yet" from "nothing is happening", and it is matched by
 * id — so a stale `pending` cannot make an unrelated run look like this one's result.
 *
 * This is the same problem `watch_step` solves in `crates/yagra-core/src/upgrade.rs`, from the other
 * side: there, core boots mid-run and must not read the first `running` as "nothing to do"; here,
 * the browser starts a run and must not read the absence of one as "already finished".
 */
export type RunPhase =
  | { kind: 'idle' }
  /** Requested, and the sidecar has not picked it up yet. */
  | { kind: 'starting' }
  | { kind: 'running'; step: UpgradeRunStep | null; index: number; total: number }
  | { kind: 'done'; state: UpgradeRunState };

export function runPhase(status: UpgradeStatus, pending: string | null): RunPhase {
  const last = status.last_run;
  if (last?.state === 'running') {
    const step = runStep(last.step);
    const index = UPGRADE_PROGRESS_STEPS.findIndex((s) => s === step);
    return { kind: 'running', step, index, total: UPGRADE_PROGRESS_STEPS.length };
  }
  // A run this browser started, which the server has now reported an outcome for.
  if (pending !== null && last?.id === pending) {
    return { kind: 'done', state: runState(last.state) ?? 'failed' };
  }
  // Asked for, but `status.json` still describes the previous run (or none). Not idle.
  if (pending !== null) return { kind: 'starting' };
  return { kind: 'idle' };
}

/** How far along the track, 0…1, or `null` when there is no determinate position.
 *
 *  `null` rather than 0 for an unrecognised step: a bar pinned at the left is a claim that nothing
 *  has happened, and "this build does not know this phase" is not that claim. */
export function stepProgress(phase: RunPhase): number | null {
  if (phase.kind === 'done') return 1;
  if (phase.kind !== 'running' || phase.index < 0) return null;
  // Count the current step as reached, not as completed: `verify` is the last phase and the run is
  // not over while it is showing.
  return (phase.index + 1) / (phase.total + 1);
}

/** Should the page keep re-reading the status?
 *
 *  Driven by **the act of starting**, not by what the server currently says — the inversion that
 *  was the bug. `DiscoveryPage` arms its interval the same way. */
export function shouldPoll(status: UpgradeStatus, pending: string | null): boolean {
  const phase = runPhase(status, pending);
  return phase.kind === 'starting' || phase.kind === 'running';
}

/** When the updater last managed to read the registry, in unix seconds.
 *
 *  ⚠️ Not `updater.last_seen`, which is the heartbeat and is always fresh — it says the sidecar is
 *  alive, never that the release list is current. Confusing the two is what would let a list 18
 *  hours stale present itself as up to date. */
export function lastChecked(status: UpgradeStatus): number | null {
  return status.available?.written_at ?? null;
}

/** May the apply button be offered at all?
 *
 *  `enabled` already folds in the switch and the sidecar's liveness — it is the backend's own
 *  answer to "could a request be accepted right now", so this does not re-derive it.
 *
 *  ⚠️ **`pending` is part of the answer, not decoration.** For the 5–10 seconds between core
 *  accepting a request and the sidecar writing a status for it, `isRunning` is false *and the
 *  backend's own 409 is false too* — both read the same `status.json`. Without this the operator
 *  can click a second release in that window and the request file is simply overwritten. */
export function canApply(status: UpgradeStatus, pending: string | null = null): boolean {
  return status.enabled && !shouldPoll(status, pending);
}

/** Which way a release moves this deployment. The backend decides — see `release_offers` in
 *  `crates/yagra-core/src/upgrade.rs`; comparing versions here would be a second copy of the rule
 *  that says whether a rollback is safe. `as const` so `i18nEnumKeys.test.ts` can iterate it. */
export const UPGRADE_OFFER_DIRECTIONS = ['upgrade', 'rollback'] as const;
export type OfferDirection = (typeof UPGRADE_OFFER_DIRECTIONS)[number];

/** Why a release cannot be installed. Same source, same reason. */
export const UPGRADE_OFFER_BLOCKS = ['below_floor', 'unknown', 'pollers_behind'] as const;
export type OfferBlock = (typeof UPGRADE_OFFER_BLOCKS)[number];

export type Offer = UpgradeStatus['offers'][number];

/** The id the core+WebUI row carries. Mirrors `CORE_COMPONENT_ID` in `crates/yagra-core/src/upgrade.rs`. */
export const CORE_ID = 'core';

/** What a components row describes. `as const` so `i18nEnumKeys.test.ts` can iterate it. */
export const COMPONENT_KINDS = ['core', 'poller'] as const;
export type ComponentKind = (typeof COMPONENT_KINDS)[number];

/** Why a row is not an ordinary, independently selectable one. */
export const COMPONENT_REASONS = ['no_site_updater', 'co_located', 'offline'] as const;
export type ComponentReason = (typeof COMPONENT_REASONS)[number];

/** Where one poller has got to in a convergence. */
export const CONVERGE_STATES = [
  'waiting',
  'prefetching',
  'applying',
  'returned',
  'failed',
  'skipped',
] as const;
export type ConvergeState = (typeof CONVERGE_STATES)[number];

export type ComponentRow = UpgradeStatus['components'][number];
export type Convergence = NonNullable<UpgradeStatus['poller_convergence']>;
export type ConvergingTarget = Convergence['targets'][number];

/** Read a reason defensively — a newer core may name one this build has never heard of.
 *
 *  Not decoration: the Tier1 mocks are generated from the OpenAPI document and fill enums with
 *  whatever the generator produces, so a reader that trusts the value renders a raw `t()` key and
 *  the walk stays green. Same discipline as `runState` and `runStep`. */
export function componentReason(raw: string | null | undefined): ComponentReason | null {
  return COMPONENT_REASONS.includes(raw as ComponentReason) ? (raw as ComponentReason) : null;
}

/** The same, for a convergence row's state. */
export function convergeState(raw: string | null | undefined): ConvergeState | null {
  return CONVERGE_STATES.includes(raw as ConvergeState) ? (raw as ConvergeState) : null;
}

/**
 * What one row would do in an upgrade to `target`.
 *
 * `already` is decided here rather than by the server, and deliberately: it depends on which
 * release the operator picked, so a row's meaning would otherwise change with the target while the
 * row itself did not. It is a string comparison — the `v` is a tag convention, not part of the
 * version — and every comparison that needs *ordering* stays in Rust (see `moves_back`).
 */
export type RowPlan = 'moves' | 'already' | 'blocked';

export function rowPlan(row: ComponentRow, target: string): RowPlan {
  if (!row.upgradable) return 'blocked';
  const bare = (v: string) => v.replace(/^v/, '');
  if (typeof row.version === 'string' && bare(row.version) === bare(target)) return 'already';
  return 'moves';
}

/**
 * Whether the operator may change this row's checkbox.
 *
 * Two reasons, and they read differently on screen: a co-located poller is replaced by core's own
 * compose project, so it follows core's box rather than having one of its own; anything else locked
 * here simply cannot move. Both are rendered with the reason **in the row** — a control whose
 * explanation is only in a tooltip is one a touch device never explains (ADR-055 R4).
 */
export function rowLocked(row: ComponentRow, target: string): boolean {
  return row.co_located || rowPlan(row, target) !== 'moves';
}

/**
 * The checkboxes as the dialog opens: everything that would move, ticked.
 *
 * The co-located poller is not in the set — it has no independent state, it follows core. Rows that
 * cannot move are not in it either, which is what makes "selected" mean the same thing as "will be
 * upgraded" and keeps the footer count honest.
 */
export function defaultSelection(rows: ComponentRow[], target: string): string[] {
  return rows.filter((r) => !rowLocked(r, target)).map((r) => r.id);
}

/**
 * Pools that lose their last live poller for **this** selection.
 *
 * 🚨 This used to be a field on the response, computed once from everything that could move. That
 * stops being true the moment a row can be unchecked, and a warning that does not follow the
 * checkboxes is worse than no warning: it reads exactly the same when it matters as when it does
 * not (ADR-051 Inc.6).
 *
 * Safe to compute here because it is **counting, not ordering** — `live_in_pool` arrives per row.
 * Anything that needs semver stays in Rust, for the reason `moves_back` gives.
 */
export function darkPools(rows: ComponentRow[], selected: string[]): string[] {
  const out = new Set<string>();
  for (const r of rows) {
    const pool = r.pool;
    if (typeof pool !== 'string' || !selected.includes(r.id)) continue;
    if (r.live_in_pool <= 1) out.add(pool);
  }
  return [...out].sort();
}

/**
 * Sites in **this** selection that have not said an upgrade is safe there.
 *
 * The sibling of `darkPools`, and it follows the checkboxes for the same reason: a warning computed
 * once from everything that could move reads exactly the same when it matters as when it does not
 * (ADR-051 Inc.6/Inc.7). Unticking a site has to remove it from the sentence, or the sentence stops
 * being about the button.
 *
 * The judgement itself is the server's — `needs_site_prep` arrives per row, decided from what the
 * site's own updater declared. Nothing here infers it from a version, and nothing may: the hazard
 * lives in the site's composition, which a poller upgraded on its own leaves untouched.
 */
export function unpreparedSites(rows: ComponentRow[], selected: string[]): string[] {
  return rows
    .filter((r) => r.needs_site_prep && selected.includes(r.id))
    .map((r) => r.id)
    .sort();
}

/**
 * How the poller half of an upgrade is going.
 *
 * `pending` is the run id the last press returned, and it does the same job it does for core's own
 * run: **there is a window in which the server can say nothing at all.** The convergence is started
 * by a spawned task, so for a moment after the 202 the snapshot is still the *previous* one — or
 * absent. Reading the server alone there would render "nothing is happening" over a press that was
 * accepted.
 *
 * ⚠️ It never decides the outcome. It only separates "asked, not visible yet" from "idle", and it
 * is matched by id, so a stale `pending` cannot make an older convergence look like this one.
 */
export type ConvergePhase =
  | { kind: 'idle' }
  | { kind: 'starting' }
  | { kind: 'running'; conv: Convergence; done: number; failed: number; total: number }
  | { kind: 'done'; conv: Convergence; done: number; failed: number; total: number };

export function convergePhase(status: UpgradeStatus, pending: string | null): ConvergePhase {
  const conv = status.poller_convergence ?? null;
  if (conv === null) return pending === null ? { kind: 'idle' } : { kind: 'starting' };
  const done = conv.targets.filter((t) => t.state === 'returned').length;
  const failed = conv.targets.filter(
    (t) => t.state === 'failed' || t.state === 'skipped',
  ).length;
  const counts = { conv, done, failed, total: conv.targets.length };
  if (conv.finished_at === null) return { kind: 'running', ...counts };
  // A press this browser made that the server has not caught up with yet: the snapshot on screen
  // is the *previous*, finished convergence, so saying "done" would report someone else's outcome
  // as the answer to the button just pressed.
  if (pending !== null && conv.run_id !== pending) return { kind: 'starting' };
  return { kind: 'done', ...counts };
}

/** How far along, 0…1, or `null` when there is no determinate position yet. */
export function convergeProgress(phase: ConvergePhase): number | null {
  if (phase.kind === 'idle' || phase.kind === 'starting') return null;
  if (phase.total === 0) return null;
  return (phase.done + phase.failed) / phase.total;
}

/** Should the page keep re-reading while pollers converge?
 *
 *  Its own beat, slower than the run poll: nothing here is being recreated underneath this browser,
 *  and a convergence runs for minutes to tens of minutes. */
export function shouldPollConvergence(status: UpgradeStatus, pending: string | null): boolean {
  const phase = convergePhase(status, pending);
  return phase.kind === 'starting' || phase.kind === 'running';
}

/** The releases newer than the running one. */
export function upgrades(status: UpgradeStatus): Offer[] {
  return status.offers.filter((o) => o.direction === 'upgrade');
}

/** The releases older than the running one, blocked ones included.
 *
 *  Kept rather than filtered out: an operator hunting for a version needs to see that it exists and
 *  why it is refused. Dropping it would read as "that release never existed" (ADR-050 decision 10).
 */
export function rollbacks(status: UpgradeStatus): Offer[] {
  return status.offers.filter((o) => o.direction === 'rollback');
}

/** May this particular release be installed right now? */
export function canOffer(
  status: UpgradeStatus,
  offer: Offer,
  pending: string | null = null,
): boolean {
  return canApply(status, pending) && !offer.blocked;
}

/** What this deployment's migration history says about going back. */
export type Rollback =
  | { kind: 'unrestricted' }
  | { kind: 'floored'; minCore: string; reason: string; sinceVersion: number };

/**
 * No floor means every applied migration was additive, so an earlier release still runs — which is
 * the default and the truth for every migration shipped before ADR-050.
 *
 * Note the asymmetry this preserves: absence of a floor is a positive statement ("reversible"),
 * because the backend's rule is that a narrowing migration must declare itself and a consistency
 * test enforces that it did. Without that test the same `null` would have to be read as "unknown".
 */
export function rollback(status: UpgradeStatus): Rollback {
  const floor = status.schema.compat;
  if (!floor) return { kind: 'unrestricted' };
  return {
    kind: 'floored',
    minCore: floor.min_core,
    reason: floor.reason,
    sinceVersion: floor.since_version,
  };
}

/** Which kind of build is running — the distinction a version number alone cannot make.
 *  An `as const` array so `i18nEnumKeys.test.ts` can iterate it (extensibility.md §4). */
export const UPGRADE_BUILD_KINDS = ['release', 'development', 'unknown'] as const;
export type BuildKind = (typeof UPGRADE_BUILD_KINDS)[number];

/**
 * A release build and a `/flashdeploy` build of the same commit are different binaries that share a
 * source ref (ADR-036), so the profile is the only thing that separates them. Worth surfacing:
 * a green pipeline has shipped a stale binary in this repository before, and "which build is this"
 * was answerable only from a support bundle until now.
 *
 * `unknown` rather than a guess when the marker file is absent — that is a `cargo run`, which is a
 * valid way to be running and not a release.
 */
export function buildKind(profile: string | null | undefined): BuildKind {
  if (profile === 'release') return 'release';
  if (profile === undefined || profile === null || profile.trim() === '') return 'unknown';
  return 'development';
}

/**
 * May an image archive be uploaded from here (ADR-050 Increment 3)?
 *
 * Three conditions, and the third is separate from the other two on purpose: a deployment can have
 * a working updater and still refuse archives, because `docker load` installs whatever the file
 * contains. The backend says so via the updater's own heartbeat, so this reads the answer rather
 * than inferring one.
 */
export function canUploadBundle(status: UpgradeStatus, pending: string | null = null): boolean {
  return canApply(status, pending) && status.updater.allow_bundle;
}

/**
 * A typo filter for the target-tag field, and **deliberately looser than the backend's rule**.
 *
 * The real grammar is `is_valid_tag` in `crates/yagra-core/src/upgrade.rs`, and it stays the only
 * one: copying it here would put the rule that decides what reaches a root-privileged container in
 * two languages with nothing comparing them (extensibility.md §2). What justifies any check at all
 * is the size — a rejected tag after a gigabyte has already been uploaded is a wasted upload.
 *
 * So this only has to be **weaker** than the backend's rule, never different in the other
 * direction: anything the backend would accept must pass here, and the test asserts exactly that.
 */
export function looksLikeReleaseTag(tag: string): boolean {
  const t = tag.trim();
  return t.startsWith('v') && t.length > 1 && !/[\s/:@]/.test(t);
}

/**
 * Guess the tag from an archive's filename, to pre-fill the field.
 *
 * A convenience with no authority: the operator can overwrite it, and the updater checks the tag
 * against the images the archive really contains regardless of where the string came from.
 */
export function bundleTagFromFilename(name: string): string | null {
  return /v\d+\.\d+\.\d+(?:-[0-9A-Za-z]+)?/.exec(name)?.[0] ?? null;
}

/** Commit refs are displayed short; the full value stays in the DOM title. */
export function shortRef(ref: string | null | undefined, len = 12): string | null {
  const trimmed = ref?.trim();
  if (!trimmed) return null;
  return trimmed.length > len ? trimmed.slice(0, len) : trimmed;
}

/** Seconds → a coarse "3d 4h" / "12m" string. Coarse on purpose: this answers "did core restart
 *  while nobody was looking?", not "how long exactly". */
export function uptime(seconds: number): string {
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}
