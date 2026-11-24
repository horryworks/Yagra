// SPDX-License-Identifier: AGPL-3.0-only
// A threshold rule on a 0/1 metric, read as "which alert, at which severity" (ADR-264).
//
// `snmp_up` and its kind take one of two values, so a rule on one says only "alert when it shows
// the fault". The four bound boxes made the operator spell that as a number — `0.5`, because the
// comparison includes the bound and `1` would fire on the healthy value. This module turns such a
// rule into a severity and back, so the dialog can offer the choice that is actually being made.
//
// Which metrics are 0/1, and which value is their fault, comes from the generated
// `api/alertNameFlags.json` (ADR-196's `AlertName::Flag`, ADR-264 decision 3) — never from the
// name: `fortinet_vpn_tunnels_up` counts tunnels.
//
// A `.ts` on purpose: Vitest never executes a `.tsx` (`testing.md`).

import alertNameFlags from '../api/alertNameFlags.json';
import { LIVENESS_METRIC } from './format';

export type FlagSeverity = 'warning' | 'critical';

/** The bound a severity is stored as: the midpoint, so neither value sits on it. */
export const FLAG_BOUND = 0.5;

const FAULTS: Readonly<Record<string, number>> = alertNameFlags;

/** The value that is `metric`'s fault, or `null` when it is not a 0/1 metric a rule writes bounds
 *  for. Reachability is excluded: it has no bounds at all, and its severity is always critical. */
export function flagFault(metric: string): 0 | 1 | null {
  const m = metric.trim();
  if (m === LIVENESS_METRIC) return null;
  const v = FAULTS[m];
  return v === 0 || v === 1 ? v : null;
}

/** The four bounds, as numbers (absent = `undefined`). */
export interface Bounds {
  warningBelow?: number;
  criticalBelow?: number;
  warningAbove?: number;
  criticalAbove?: number;
}

/** How a rule on a 0/1 metric reads:
 *
 *  - a severity, when it names exactly one bound, on the fault's side, strictly between the two
 *    values once the comparison's own edge is counted (`≤ b` catches 0 and not 1 for `0 ≤ b < 1`);
 *  - `'unset'`, when it names no bound yet (a new rule) — the dialog waits for a choice;
 *  - `null`, for every other shape. The dialog then shows the four boxes, so a rule an operator
 *    wrote by hand is shown as written rather than rewritten (ADR-264 decision 4). */
export function flagSeverity(fault: 0 | 1, b: Bounds): FlagSeverity | 'unset' | null {
  const set = (
    [
      ['warning', 'below', b.warningBelow],
      ['critical', 'below', b.criticalBelow],
      ['warning', 'above', b.warningAbove],
      ['critical', 'above', b.criticalAbove],
    ] as const
  ).filter(([, , v]) => v !== undefined && v !== null);
  if (set.length === 0) return 'unset';
  if (set.length > 1) return null;
  const [severity, side, v] = set[0];
  const value = v as number;
  if (fault === 0) return side === 'below' && value >= 0 && value < 1 ? severity : null;
  return side === 'above' && value > 0 && value <= 1 ? severity : null;
}

/** The bounds a severity is stored as: one bound, on the fault's side, at [`FLAG_BOUND`]. */
export function flagBounds(fault: 0 | 1, severity: FlagSeverity): Bounds {
  const side = fault === 0 ? 'Below' : 'Above';
  return { [`${severity}${side}`]: FLAG_BOUND } as Bounds;
}
