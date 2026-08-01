// SPDX-License-Identifier: AGPL-3.0-only
// Tone/state mappings for the node Overview's health cards.
//
// Extracted from OverviewTab.tsx so the thresholds are testable (Vitest never runs `.tsx`). Each
// maps a raw reading onto the shared status palette, and ui-conventions require a node's status
// colour to be identical in the table, the map and the chart — so these must resolve to the
// `--status-*` variables rather than inventing a colour.

import type { NodeState } from '../../types/api';

/** A three-way tone shared by the URL/DNS health cards, keyed to the status palette. */
export type HealthTone = 'up' | 'warning' | 'critical';

/** CSS status-palette variable for a tone. Never a literal colour — the same node must read the
 *  same in the table, on the map and in a chart. */
export function httpToneVar(tone: HealthTone): string {
  if (tone === 'up') return 'var(--status-ok)';
  if (tone === 'warning') return 'var(--status-warning)';
  return 'var(--status-critical)';
}

/** Days-to-expiry bands for a TLS certificate, matching the thresholds the built-in URL-monitor
 *  profile seeds (warn under 30 days, critical under 7).
 *
 *  An already-expired certificate has negative days and must read critical, not wrap around to
 *  healthy — which is what a naive `days < 30 && days > 7` would do. */
export function certTone(days: number): HealthTone {
  if (days < 7) return 'critical';
  if (days < 30) return 'warning';
  return 'up';
}

/** ifOperStatus → node state. `1` is up (IF-MIB); anything else is a real problem, and an absent
 *  reading is `unknown` rather than being guessed either way. */
export function operState(oper: number | null): NodeState {
  if (oper == null) return 'unknown';
  return oper === 1 ? 'ok' : 'critical';
}
