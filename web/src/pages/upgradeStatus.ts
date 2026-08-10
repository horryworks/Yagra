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

/** Whether the privileged updater sidecar is present and reporting (ADR-050 decision 1). */
export type Mechanism = 'absent' | 'stopped' | 'ready';

/**
 * Three states, not two, and the middle one is the reason.
 *
 * `absent` is a deployment choice — the sidecar was never enabled, and the fix is to enable it.
 * `stopped` is a fault — it was enabled and has gone quiet, and the fix is to look at its logs.
 * Rendering those identically would train an operator to ignore the one that matters.
 *
 * All three are also kept distinct from "there is no newer version", which is a fourth fact
 * entirely. Same discipline ADR-040 applied to `/flags`: never present an inferred value as a
 * known one.
 */
export function mechanism(status: UpgradeStatus): Mechanism {
  if (!status.updater.present) return 'absent';
  return status.updater.fresh ? 'ready' : 'stopped';
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

/** May the apply button be offered at all? */
export function canApply(status: UpgradeStatus): boolean {
  return status.enabled && !isRunning(status);
}

/** Releases worth offering: everything the updater found except the one already running.
 *  Sorted by the updater; this only removes the current version so the list is a list of *moves*. */
export function offerableReleases(status: UpgradeStatus): string[] {
  const current = `v${status.current.core_version}`;
  return (status.available?.releases ?? []).map((r) => r.tag).filter((t) => t !== current);
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

/** Commit refs are displayed short; the full value stays in the DOM title. */
export function shortRef(ref: string | null | undefined, len = 12): string | null {
  const trimmed = ref?.trim();
  if (!trimmed) return null;
  return trimmed.length > len ? trimmed.slice(0, len) : trimmed;
}
