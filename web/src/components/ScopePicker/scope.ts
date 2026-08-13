// SPDX-License-Identifier: AGPL-3.0-only
// What a scope selection *is*, and how it is labelled — the pure half of `ScopePicker`, split out
// so the label logic is unit-testable without a DOM (Vitest runs `environment: 'node'` and never
// executes a `.tsx`, so judgement that lives in one is judgement nothing tests).
//
// Here rather than in `troubleshoot/`, where it was written, because it is no longer a troubleshoot
// concern: Alerts ▸ History asks the same "all / group / node" question, and a component shared
// across domains living inside one of them is the migration tripwire `api-conventions.md` describes
// for backend helpers — the frontend has the same shape. What stayed behind is what is genuinely
// analysis-specific: the quick-run window/baseline/σ defaults (`troubleshoot/analysisDefaults.ts`).
//
// i18n: the label builders take the caller's `t` rather than resolving at module load, so they
// follow the active language (a module-load `t()` would freeze one). The keys moved to `common`
// with the component — a `troubleshoot:` key read by a page that is not Troubleshoot is the same
// mistake one level down.

import type { TFunction } from 'i18next';

/** A chosen scope: everything / a group / a single node, plus a human label. */
export interface ScopeValue {
  kind: 'all' | 'group' | 'node';
  /** Group/node id; null for All. */
  id: string | null;
  /** Human label shown on the trigger, and sent as an analysis job's `scope_label` prefix. */
  label: string;
}

/** The default scope (everything), with a localized label. Built from the caller's `t` rather than
 *  a module-level const so the "All nodes" label follows the active language. */
export function allScope(t: TFunction): ScopeValue {
  return { kind: 'all', id: null, label: t('common:scope.all') };
}

/** Scope label for a group (recursive — a group scope covers its subtree, ADR-022). */
export function groupScopeLabel(name: string, t: TFunction): string {
  return t('common:scope.groupLabel', { name });
}

/** Scope label for a single node. */
export function nodeScopeLabel(name: string, t: TFunction): string {
  return t('common:scope.nodeLabel', { name });
}
