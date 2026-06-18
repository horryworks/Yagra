// Pure scope helpers used by ScopePicker — split out so the filtering and label logic is unit-
// testable without a DOM (the repo has no React test renderer; tests target pure functions).

import type { AnalysisJobInput, AnalysisToolKey, NodeSummary } from '../types/api';

/** Quick-run defaults (the split-button "Run on all nodes" path + the drawer's initial state). */
export const DEFAULT_WINDOW_SECS = 7 * 86_400;
export const DEFAULT_BASELINE_SECS = 14 * 86_400;
/** σ threshold matching the drawer's centre slider (balanced). */
export const DEFAULT_SIGMA = 3.0;

/** A "quick run" job input: every node, standard defaults — no configuration step. */
export function defaultAnalysisInput(tool: AnalysisToolKey): AnalysisJobInput {
  return {
    tool,
    scope_kind: 'all',
    scope_id: null,
    scope_label: 'All nodes · 7 d',
    window_secs: DEFAULT_WINDOW_SECS,
    baseline_secs: DEFAULT_BASELINE_SECS,
    sensitivity: DEFAULT_SIGMA,
    depth: 'standard',
    family: 'all',
    notify: true,
  };
}

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
