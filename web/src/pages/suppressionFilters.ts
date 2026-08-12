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

import { isFiltered as isFilteredAgainst, textMatch } from '../lib/filterQuery';
import { MAINTENANCE_STATUSES, windowStatus, type MaintenanceStatus } from './maintenanceStatus';
import type { MaintenanceWindow, Mute } from '../types/api';

// ─────────────────────────────────────────────────────────── maintenance windows

/** The statuses the dropdown offers, derived from the badge's own vocabulary so a fifth status
 *  cannot appear in the column and be un-filterable. */
export const MAINTENANCE_STATUS_FILTERS = MAINTENANCE_STATUSES;

export interface MaintenanceFilters {
  status: MaintenanceStatus | '';
  /** Free text over the window's name and the thing it covers. */
  q: string;
}

export const DEFAULT_MAINTENANCE_FILTERS: MaintenanceFilters = { status: '', q: '' };

/**
 * Whether one window survives the filter.
 *
 * `labelOf` resolves the scope to a human name and is consulted **only** when a term is typed: on
 * the maintenance screen it goes through `useEntityNames`, whose resolver enqueues every id it is
 * asked about, so calling it per row unconditionally would resolve names nobody is looking for.
 *
 * `now` is a parameter so the status boundary is testable without faking the clock, and so a whole
 * render uses one reading — two rows computed a millisecond apart must not disagree about a window
 * that ends between them.
 */
export function matchesWindow(
  w: MaintenanceWindow,
  f: MaintenanceFilters,
  labelOf: (w: MaintenanceWindow) => string,
  now: number,
): boolean {
  if (f.status && windowStatus(w, now).labelKey !== f.status) return false;
  if (f.q.trim() === '') return true;
  return textMatch(f.q, w.name, labelOf(w));
}

export function isMaintenanceFiltered(f: MaintenanceFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_MAINTENANCE_FILTERS);
}

// ─────────────────────────────────────────────────────────────────────── mutes

/** A mute is either still silencing something or has run out. The server prunes expired ones, so
 *  this mostly separates "what is in force now" from a row the page has not yet reloaded past —
 *  which is exactly the question an operator asks when a notification did not arrive. */
export const MUTE_STATES = ['active', 'expired'] as const;
export type MuteState = (typeof MUTE_STATES)[number];

export interface MuteFilters {
  state: MuteState | '';
  /** Free text over the target, the metric and the reason. */
  q: string;
}

export const DEFAULT_MUTE_FILTERS: MuteFilters = { state: '', q: '' };

/** Whether a mute has run out. `<=` matches the server's own boundary, the way `isEnded` does for
 *  a window — a mute expiring exactly now must not land on opposite sides of the two clocks. */
export function muteIsExpired(m: Pick<Mute, 'until_at'>, now: number): boolean {
  return new Date(m.until_at).getTime() <= now;
}

/** Whether one mute survives the filter. `targetOf` is consulted only when a term is typed, for
 *  the same reason as {@link matchesWindow}. */
export function matchesMute(
  m: Mute,
  f: MuteFilters,
  targetOf: (m: Mute) => string,
  now: number,
): boolean {
  if (f.state === 'active' && muteIsExpired(m, now)) return false;
  if (f.state === 'expired' && !muteIsExpired(m, now)) return false;
  if (f.q.trim() === '') return true;
  return textMatch(f.q, targetOf(m), m.metric_name, m.reason);
}

export function isMuteFiltered(f: MuteFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_MUTE_FILTERS);
}
