// Pure scope helpers used by ScopePicker — split out so the filtering and label logic is unit-
// testable without a DOM (the repo has no React test renderer; tests target pure functions).

import type { NodeSummary } from '../types/api';

/** A chosen analysis scope: All nodes / a group / a single node, plus a human label. */
export interface ScopeValue {
  kind: 'all' | 'group' | 'node';
  /** Group/node id; null for All. */
  id: string | null;
  /** Human label shown on the trigger and sent as the job's scope_label prefix. */
  label: string;
}

/** The default scope (every node). */
export const ALL_SCOPE: ScopeValue = { kind: 'all', id: null, label: 'All nodes' };

/** Case-insensitive substring match of nodes by name OR address (operators search by both). */
export function filterNodes(nodes: NodeSummary[], query: string): NodeSummary[] {
  const q = query.trim().toLowerCase();
  if (!q) return nodes;
  return nodes.filter(
    (n) => n.name.toLowerCase().includes(q) || n.address.toLowerCase().includes(q),
  );
}

/** Scope label for a group (recursive — a group scope covers its subtree, ADR-022). */
export function groupScopeLabel(name: string): string {
  return `group: ${name} (incl. subgroups)`;
}

/** Scope label for a single node. */
export function nodeScopeLabel(name: string): string {
  return `node: ${name}`;
}
