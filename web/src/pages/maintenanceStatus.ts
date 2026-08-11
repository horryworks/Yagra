// SPDX-License-Identifier: AGPL-3.0-only
// A maintenance window's display status, and the predicate the bulk clear counts.
//
// Extracted from MaintenancePage.tsx so the precedence is testable (Vitest never runs `.tsx`).
// The order matters and is not obvious:
//
//   - **`ended` comes first.** A window whose end time has passed is finished whether or not it
//     was ever enabled — before this, a window that was switched off and then ran out sat in the
//     list reading "disabled" forever, indistinguishable from one waiting to be turned on.
//   - **then `disabled`.** A window that has *not* ended reads as disabled even while its clock
//     says it is running, because the server does not suppress alerts for it — showing "active"
//     there would tell an operator their alerts are muted when they are not.
//
// `isEnded` mirrors the server's `ends_at <= now()` (see `MaintenanceRepo::delete_ended_windows`),
// including the `<=`: the server's `active` flag is `ends_at > now()`, so a `<` here would put a
// window ending exactly now on the opposite side of the boundary from the server.

import type { MaintenanceWindow } from '../types/api';

/** Every status a window can display as. `as const` rather than a bare `string` return so
 *  `i18nEnumKeys.test.ts` can demand `maintenance.status.*` in both locales — the page interpolates
 *  this key with no fallback, and a fifth status would otherwise render raw in the column an
 *  operator reads to check whether alerting is currently suppressed. */
export const MAINTENANCE_STATUSES = ['disabled', 'active', 'ended', 'scheduled'] as const;

/** A window's display status. */
export type MaintenanceStatus = (typeof MAINTENANCE_STATUSES)[number];

/** Whether a window has finished — the badge's `ended` case, and exactly what the bulk clear
 *  counts and the server deletes.
 *
 *  `!w.active` is the load-bearing term: `active` is the server's own statement that this window
 *  is suppressing alerts right now, so a stale page or a skewed browser clock can only ever
 *  *under*-count. It must never offer to delete a live suppression. */
export function isEnded(w: MaintenanceWindow, now: number = Date.now()): boolean {
  return !w.active && new Date(w.ends_at).getTime() <= now;
}

/** Language-agnostic status key + badge tone; the label is resolved at the call site. */
export function windowStatus(
  w: MaintenanceWindow,
  now: number = Date.now(),
): { labelKey: MaintenanceStatus; tone: 'info' | 'neutral' } {
  if (isEnded(w, now)) return { labelKey: 'ended', tone: 'neutral' };
  if (!w.enabled) return { labelKey: 'disabled', tone: 'neutral' };
  if (w.active) return { labelKey: 'active', tone: 'info' };
  return { labelKey: 'scheduled', tone: 'neutral' };
}
