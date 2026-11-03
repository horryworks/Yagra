// SPDX-License-Identifier: AGPL-3.0-only
// What the node-detail Alert rules tab shows (ADR-229): one row per metric, the rules in force on
// it, and the ones a narrower rule took over.
//
// A `.ts` on purpose: Vitest never executes a `.tsx` (testing.md), and the grouping and the
// "which rule does the button edit" answer are the two decisions this tab makes.

import { LIVENESS_METRIC } from '../../lib/format';
import { thresholdFormFrom, type ThresholdForm } from '../../pages/thresholdRequest';
import type { MatchingThreshold, StoredThreshold } from '../../types/api';

/** One metric's rules on one node. */
export interface NodeRuleRow {
  metric: string;
  /** The rules at the winning level, most specific first as the server sent them. More than one
   *  when several rules sit at that level: the engine keeps the stricter bound of each side. */
  inForce: StoredThreshold[];
  /** The rules that reach the node but lose to a narrower one. */
  overridden: StoredThreshold[];
}

/** Group the server's answer by metric. Liveness first — it is the rule that says the node is
 *  down — then the rest by name, so the list reads the same on every node. */
export function nodeRuleRows(rows: readonly MatchingThreshold[]): NodeRuleRow[] {
  const byMetric = new Map<string, NodeRuleRow>();
  for (const { rule, in_force } of rows) {
    let row = byMetric.get(rule.metric);
    if (!row) {
      row = { metric: rule.metric, inForce: [], overridden: [] };
      byMetric.set(rule.metric, row);
    }
    (in_force ? row.inForce : row.overridden).push(rule);
  }
  return [...byMetric.values()].sort((a, b) => {
    if (a.metric === b.metric) return 0;
    if (a.metric === LIVENESS_METRIC) return -1;
    if (b.metric === LIVENESS_METRIC) return 1;
    return a.metric < b.metric ? -1 : 1;
  });
}

/** The in-force rule that is this node's own and names nothing else — the one the row's button
 *  edits. `null` when the rule in force is inherited, or names other nodes too: editing a shared
 *  rule from one node's screen would change the others without saying so. */
export function ownRule(row: NodeRuleRow, nodeId: string): StoredThreshold | null {
  return (
    row.inForce.find(
      (r) => r.scope_level === 'node' && r.scope_ids.length === 1 && r.scope_ids[0] === nodeId,
    ) ?? null
  );
}

/** The form a new rule for this node alone starts from: the bounds in force now, at the node
 *  level, on this node. The operator changes what should differ and saves a narrower rule. */
export function nodeOverrideForm(row: NodeRuleRow, nodeId: string): ThresholdForm {
  const from = row.inForce[0] ?? row.overridden[0];
  return { ...thresholdFormFrom(from), level: 'node', scopeIds: [nodeId], metric: row.metric };
}
