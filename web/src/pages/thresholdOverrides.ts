// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Metric alert rules — which rows are overridden by a narrower rule on the same metric
// (ADR-200 Inc.14). It replaces the page note "the most specific scope wins": instead of saying the
// rule, the row that loses somewhere says where.
//
// Only overrides the list can PROVE are reported. A rule for "every node" loses to any narrower rule
// on its metric, and a node rule loses to a port rule on one of its nodes — both are facts about the
// two rows alone. Whether a node rule beats a profile or folder rule depends on which profile and
// folder that node is in, which the rule list does not carry, so those pairs are left unmarked: the
// badge may say less than the truth, never something false.
//
// It reads the rows on screen (the operator's filter, capped by the server), so a narrower rule
// filtered out of view is not counted. Same direction: an undercount, never an invented override.

import { splitInterfaceScopeId } from '../lib/interfaceScope';
import type { StoredThreshold } from '../types/api';

/** How a row is overridden. `nodes` when every overriding rule names nodes or ports (so the nodes
 *  can be counted); otherwise `rules`, the number of narrower rules. */
export type Override = { kind: 'nodes'; count: number } | { kind: 'rules'; count: number };

/** Whether two row patterns (ADR-143) can reach the same row. A blank pattern reaches every row; two
 *  different patterns are treated as disjoint even though wildcards might overlap — the undercount
 *  side again. Case is ignored, as the engine ignores it. */
function rowsOverlap(a: string | null | undefined, b: string | null | undefined): boolean {
  if (!a || !b) return true;
  return a.toLowerCase() === b.toLowerCase();
}

/** The node ids a node- or port-scoped rule touches. */
function nodesOf(rule: StoredThreshold): string[] {
  return rule.scope_level === 'interface'
    ? rule.scope_ids.map((id) => splitInterfaceScopeId(id)[0])
    : rule.scope_ids;
}

/** Whether `narrow` certainly takes over from `broad` somewhere. */
function overrides(broad: StoredThreshold, narrow: StoredThreshold): boolean {
  if (narrow.id === broad.id || narrow.metric !== broad.metric) return false;
  if (!rowsOverlap(broad.row_match, narrow.row_match)) return false;
  if (broad.scope_level === 'global') return narrow.scope_level !== 'global';
  if (broad.scope_level === 'node' && narrow.scope_level === 'interface') {
    return nodesOf(narrow).some((n) => broad.scope_ids.includes(n));
  }
  return false;
}

/** Each overridden row's id, with how it is overridden. Rows that are not overridden are absent. */
export function overriddenRows(rows: StoredThreshold[]): Map<string, Override> {
  const out = new Map<string, Override>();
  for (const broad of rows) {
    const by = rows.filter((r) => overrides(broad, r));
    if (by.length === 0) continue;
    const countable = by.every((r) => r.scope_level === 'node' || r.scope_level === 'interface');
    if (!countable) {
      out.set(broad.id, { kind: 'rules', count: by.length });
      continue;
    }
    const nodes = new Set(by.flatMap(nodesOf));
    // A node rule is overridden only on its own nodes, not on every node a port rule names.
    if (broad.scope_level === 'node') {
      for (const n of [...nodes]) if (!broad.scope_ids.includes(n)) nodes.delete(n);
    }
    out.set(broad.id, { kind: 'nodes', count: nodes.size });
  }
  return out;
}
