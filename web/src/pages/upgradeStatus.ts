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
export type Mechanism = 'absent' | 'stopped' | 'paused' | 'ready';

/**
 * Four states, and every boundary between them earns its place.
 *
 * `absent` — no sidecar has ever reported. Since it now ships in the composition, this means it was
 *   removed, or the stack has not finished starting.
 * `stopped` — it reported once and has gone quiet. A fault; the fix is to read its logs.
 * `paused` — alive, and the operator switched the mechanism off. A choice, not a fault.
 * `ready` — alive and permitted.
 *
 * Rendering any two of these the same would train an operator to ignore the one that matters. All
 * four are also distinct from "there is no newer version", which is a fifth fact entirely — same
 * discipline ADR-040 applied to `/flags`: never present an inferred value as a known one.
 *
 * Reads `upgrade_enabled` (what this deployment stored) rather than `updater.paused` (what the
 * sidecar last saw). They converge within one beat, and the stored value is the one the toggle
 * shows, so following it keeps the switch from appearing to bounce back after a click.
 */
export function mechanism(status: UpgradeStatus): Mechanism {
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

/** May the apply button be offered at all?
 *
 *  `enabled` already folds in the switch and the sidecar's liveness — it is the backend's own
 *  answer to "could a request be accepted right now", so this does not re-derive it. */
export function canApply(status: UpgradeStatus): boolean {
  return status.enabled && !isRunning(status);
}

/** Which way a release moves this deployment. The backend decides — see `release_offers` in
 *  `crates/yagra-core/src/upgrade.rs`; comparing versions here would be a second copy of the rule
 *  that says whether a rollback is safe. `as const` so `i18nEnumKeys.test.ts` can iterate it. */
export const UPGRADE_OFFER_DIRECTIONS = ['upgrade', 'rollback'] as const;
export type OfferDirection = (typeof UPGRADE_OFFER_DIRECTIONS)[number];

/** Why a release cannot be installed. Same source, same reason. */
export const UPGRADE_OFFER_BLOCKS = ['below_floor', 'unknown'] as const;
export type OfferBlock = (typeof UPGRADE_OFFER_BLOCKS)[number];

export type Offer = UpgradeStatus['offers'][number];

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
export function canOffer(status: UpgradeStatus, offer: Offer): boolean {
  return canApply(status) && !offer.blocked;
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
export function canUploadBundle(status: UpgradeStatus): boolean {
  return canApply(status) && status.updater.allow_bundle;
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
