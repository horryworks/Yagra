// SPDX-License-Identifier: AGPL-3.0-only
// A maintenance window's display status.
//
// Extracted from MaintenancePage.tsx so the precedence is testable (Vitest never runs `.tsx`).
// The order matters and is not obvious: a *disabled* window reads as disabled even while its
// clock says it is running, because the server does not suppress alerts for it — showing "active"
// there would tell an operator their alerts are muted when they are not.

import type { MaintenanceWindow } from '../types/api';

/** Language-agnostic status key + badge tone; the label is resolved at the call site. */
export function windowStatus(
  w: MaintenanceWindow,
  now: number = Date.now(),
): { labelKey: string; tone: 'info' | 'neutral' } {
  if (!w.enabled) return { labelKey: 'disabled', tone: 'neutral' };
  if (w.active) return { labelKey: 'active', tone: 'info' };
  if (new Date(w.ends_at).getTime() < now) return { labelKey: 'ended', tone: 'neutral' };
  return { labelKey: 'scheduled', tone: 'neutral' };
}
