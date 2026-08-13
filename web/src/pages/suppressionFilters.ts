// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the two suppression screens show — Alerts ▸ Maintenance windows and Alerts ▸ Mutes.
//
// Client-side, and permitted to be: both lists are bounded by what an operator typed in, not by
// fleet size (`ui-conventions.md`, "scale-aware lists"). Which makes this the only thing deciding
// what an operator sees, so it lives in a `.ts` where a test can reach it (testing.md).
//
// One module for both because they are the same question asked twice — "is this suppression in
// force, and which one is it" — and answering it in two files is how the two screens would end up
// disagreeing about what "expired" means.
//
// **What is left here is what a column reads off a row, and nothing else.** The two hand-written
// `matchesX(row, filters)` predicates are gone (ADR-053 Inc.5): the row test is now
// `lib/filterPredicate.ts::buildPredicate`, shared by every screen. Two things came free with that
// and are worth knowing, because both used to be this file's job: the columns are ANDed, and an
// inactive column never reads its row — which is what keeps `useEntityNames` from being asked to
// resolve a name for every row on screen.

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import { MAINTENANCE_STATUSES, windowStatus } from './maintenanceStatus';
import type { MaintenanceWindow, Mute } from '../types/api';

// ─────────────────────────────────────────────────────────── maintenance windows

/** The statuses the dropdown offers, derived from the badge's own vocabulary so a fifth status
 *  cannot appear in the column and be un-filterable. */
export const MAINTENANCE_STATUS_FILTERS = MAINTENANCE_STATUSES;

// ─────────────────────────────────────────────────────────────────────── mutes

/** A mute is either still silencing something or has run out. The server prunes expired ones, so
 *  this mostly separates "what is in force now" from a row the page has not yet reloaded past —
 *  which is exactly the question an operator asks when a notification did not arrive. */
export const MUTE_STATES = ['active', 'expired'] as const;
export type MuteState = (typeof MUTE_STATES)[number];

/** Whether a mute has run out. `<=` matches the server's own boundary, the way `isEnded` does for
 *  a window — a mute expiring exactly now must not land on opposite sides of the two clocks. */
export function muteIsExpired(m: Pick<Mute, 'until_at'>, now: number): boolean {
  return new Date(m.until_at).getTime() <= now;
}

// ───────────────────────────────────────────── the filter row (ADR-053 Inc.5)
//
// Both screens moved from a toolbar (one search box + one status dropdown) to a filter row under
// the header, which is strictly more expressive: the free-text box searched several fields at once
// and could not say *which*, so "prod" matched a window named prod and a window covering the prod
// group identically. Per-column conditions say which.
//
// ⚠️ `labelOf` / `targetOf` resolve an id through `useEntityNames`, whose resolver **enqueues every
// id it is asked about**. `buildPredicate` compiles an inactive column to `null` and never reads a
// row through it, so these are consulted only while a term is typed — the same property the old
// `f.q.trim() === ''` early return bought, now a consequence of the shared predicate rather than
// something each `matchesX` had to remember.

/** The Alerts ▸ Mutes filter row, keyed by `Column.key`. */
export function muteFilters(
  t: TFunction,
  targetOf: (m: Mute) => string,
  nowMs: number,
): Record<string, ColumnFilterSpec<Mute>> {
  return {
    target: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (m) => [targetOf(m)],
      containsSemantics: 'substring',
      placeholder: t('mutes.cols.target'),
    },
    metric: {
      kind: 'text',
      modes: ['contains', 'regex'],
      // A mute with no metric silences everything for its target, and a group mute has no metric at
      // all. Both read as empty here rather than as the badge's words: matching the *label* would
      // make "all" a search term that finds every unrestricted mute, which is a different question
      // from "which mutes name a metric containing this".
      readText: (m) => [m.metric_name ?? ''],
      containsSemantics: 'substring',
      placeholder: t('mutes.cols.metric'),
    },
    // The `until` column shows a timestamp; what an operator asks of it is "is this still in
    // force". That is the old dropdown, now under the column whose value decides it.
    until: {
      kind: 'enum',
      options: MUTE_STATES.map((s) => ({ value: s, label: t(`mutes.state.${s}`) })),
      readValue: (m) => (muteIsExpired(m, nowMs) ? 'expired' : 'active'),
      allLabel: t('mutes.filter.allStates'),
      counts: 'client',
    },
    reason: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (m) => [m.reason],
      containsSemantics: 'substring',
      placeholder: t('mutes.cols.reason'),
    },
  };
}

/** The Alerts ▸ Maintenance filter row, keyed by `Column.key`. */
export function windowFilters(
  t: TFunction,
  labelOf: (w: MaintenanceWindow) => string,
  nowMs: number,
): Record<string, ColumnFilterSpec<MaintenanceWindow>> {
  return {
    name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (w) => [w.name],
      containsSemantics: 'substring',
      placeholder: t('maintenance.cols.name'),
    },
    scope: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (w) => [labelOf(w)],
      containsSemantics: 'substring',
      placeholder: t('maintenance.cols.scope'),
    },
    status: {
      kind: 'enum',
      // Read off the badge's own vocabulary, so a fifth status cannot appear in the column and be
      // un-filterable.
      options: MAINTENANCE_STATUS_FILTERS.map((s) => ({
        value: s,
        label: t(`maintenance.status.${s}`),
      })),
      readValue: (w) => windowStatus(w, nowMs).labelKey,
      allLabel: t('maintenance.filter.allStatuses'),
      counts: 'client',
    },
  };
}
