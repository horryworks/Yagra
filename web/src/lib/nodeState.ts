// SPDX-License-Identifier: AGPL-3.0-only
// The NodeState vocabulary: every state and the two orders they are read in. One file so adding a
// state is one edit instead of five.
//
// The two orders are genuinely different concepts, not a copy-paste accident — keep them distinct:
//  - DISPLAY_ORDER  best → worst, neutral trailing. Health-bar segments and their legend, where a
//                   stable left-to-right reading matters more than urgency.
//  - SEVERITY_ORDER worst → best. Roll-ups ("what is this group's state?") and any list where the
//                   thing needing attention must come first.
// These lived in four places under three names (`STATE_ORDER` in lib/nodeTree, `SEVERITY_ORDER` in
// dashboard/widgets/util, `ORDER` in StatusSummary, `LEGEND_ORDER` in TopologyMapPage) — three of
// them byte-identical — plus a fifth hand-written list validating the SSE stream.

import type { NodeState } from '../types/api';

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

/** The "hard-down" states every down-KPI merges (nodes-down tile, fleet timeline's Down line,
 *  the availability ratio's denominator on the backend): a hard failure or an unreachable node.
 *  Subset of {@link PROBLEM_STATES}, which adds `warning`. */
export const HARD_DOWN_STATES: readonly NodeState[] = ['critical', 'unreachable'];

/** Narrow an untrusted string (SSE payload, URL param) to a `NodeState`. */
export function isNodeState(s: string): s is NodeState {
  return (SEVERITY_ORDER as readonly string[]).includes(s);
}

/** A zeroed per-state tally — the shape both the client-side count and the server's
 *  `fleet/group-summary` rollup use. */
export function emptyStateCounts(): Record<NodeState, number> {
  return Object.fromEntries(SEVERITY_ORDER.map((s) => [s, 0])) as Record<NodeState, number>;
}
