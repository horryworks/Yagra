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
 *  This used to carry a long note about `severity` being a bare `String` on `StoredEventRule`,
 *  `AlertHistoryRow` and `AlertTransition`, which made the closed union a WebUI belief rather than a
 *  contract. **That is no longer true** — all three are typed, so the generated types narrow them
 *  and the degradation happens once, server-side, where the row is read.
 *
 *  What remains is genuinely untrusted input: an SSE payload or a URL parameter, neither of which
 *  the contract can vouch for. Its companion `asSeverity` had exactly one shape of caller left
 *  (a fallback to `info`) and no callers at all once the DTOs were typed, so it is gone. */
export function isSeverity(v: string): v is Severity {
  return (SEVERITIES as readonly string[]).includes(v);
}

/** A zeroed per-state tally — the shape both the client-side count and the server's
 *  `fleet/group-summary` rollup use. */
export function emptyStateCounts(): Record<NodeState, number> {
  return Object.fromEntries(SEVERITY_ORDER.map((s) => [s, 0])) as Record<NodeState, number>;
}
