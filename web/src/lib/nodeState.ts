// SPDX-License-Identifier: AGPL-3.0-only
// The NodeState vocabulary: every state, the two orders they are read in, and the wire-validation
// guard. One file so adding a state is one edit instead of five.
//
// The two orders are genuinely different concepts, not a copy-paste accident — keep them distinct:
//  - DISPLAY_ORDER  best → worst, neutral trailing. Health-bar segments and their legend, where a
//                   stable left-to-right reading matters more than urgency.
//  - SEVERITY_ORDER worst → best. Roll-ups ("what is this group's state?") and any list where the
//                   thing needing attention must come first.
// These lived in four places under three names (`STATE_ORDER` in lib/nodeTree, `SEVERITY_ORDER` in
// dashboard/widgets/util, `ORDER` in StatusSummary, `LEGEND_ORDER` in TopologyMapPage) — three of
// them byte-identical — plus a fifth hand-written list validating the SSE stream.

import { SEVERITIES } from '../types/api';
import type { NodeState, Severity } from '../types/api';

/** Every node state, in severity order (worst first). The single enumeration of the union. */
export const SEVERITY_ORDER: readonly NodeState[] = [
  'critical',
  'unreachable',
  'warning',
  'unknown',
  'maintenance',
  'ok',
];

/** The order states are shown in a health bar / legend (best → worst, with the neutral states
 *  trailing). Stable so the bar segments and legend read consistently everywhere. */
export const DISPLAY_ORDER: readonly NodeState[] = [
  'ok',
  'warning',
  'critical',
  'unreachable',
  'maintenance',
  'unknown',
];

/** States that mean a node "needs attention" (surfaced in red counts on group rollups). */
export const PROBLEM_STATES: ReadonlySet<NodeState> = new Set<NodeState>([
  'warning',
  'critical',
  'unreachable',
]);

/** Narrow an untrusted string (SSE payload, URL param) to a `NodeState`. */
export function isNodeState(s: string): s is NodeState {
  return (SEVERITY_ORDER as readonly string[]).includes(s);
}

/** Whether an untrusted string is a severity this build knows.
 *
 *  Needed because Rust types `severity` as a bare `String` on `StoredEventRule`, `AlertHistoryRow`
 *  and `AlertTransition`, so the generated contract offers `string` and the closed union is a WebUI
 *  belief that a DB `CHECK` happens to uphold. A core newer than this bundle can name a variant the
 *  display helpers have no case for, and those helpers are exhaustive switches with no `default` —
 *  so an unguarded value reaches them and paints a colourless dot.
 *
 *  The membership test lives here and the *policy* stays at the call site, because callers want
 *  different ones: a badge reads an unknown severity as `info` (what the backend itself does —
 *  `events.rs::parse_severity`), while a dashboard dot would rather show neutral than claim a
 *  severity it did not receive. It arrived as three copies in one change, which is what
 *  `extensibility.md` §3 is about. */
export function isSeverity(v: string): v is Severity {
  return (SEVERITIES as readonly string[]).includes(v);
}

/** An alert severity off the wire, falling back to `info` — see [`isSeverity`] for why. */
export function asSeverity(value: string): Severity {
  return isSeverity(value) ? value : 'info';
}

/** A zeroed per-state tally — the shape both the client-side count and the server's
 *  `fleet/group-summary` rollup use. */
export function emptyStateCounts(): Record<NodeState, number> {
  return Object.fromEntries(SEVERITY_ORDER.map((s) => [s, 0])) as Record<NodeState, number>;
}
