// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Active — which alerts the triage table shows, as pure functions.
//
// Here rather than in the page for the usual reason: Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so a test written beside a `.tsx` is a file nothing runs
// (testing.md). The page keeps the layout; every decision about what an operator sees is below.
//
// ⚠️ **This filters in the browser, and that is an exception, not a relaxation of the rule.**
// `ui-conventions.md` classes alert lists with the scale-aware ones that must filter server-side.
// The reason this one may not is specific: the active-alert set is already whole in the browser —
// `useAlertStream` maintains it over SSE and `useAlertStore` holds every open alert — so there is
// no page boundary for a filter to hide older matches behind, which is the failure the server-side
// rule exists to prevent (and which Alerts ▸ History and Settings ▸ Audit actually had). Moving it
// server-side would mean giving the stream a subscription filter — an SSE design change, ADR
// territory — not adding a query parameter.
//
// **ADR-243 put the screen on the shared table with History's columns.** The filters are the
// columns History filters — severity, node, what, state — under History's URL keys, so a link moves
// between the two screens by changing the path. The scope (all / a folder / a node) is History's
// too, read from the same `node_id` / `group_id` keys, but decided here in the browser.

import { alertSubject, subjectNodeId, subjectText, type HasSubject } from '../lib/alertSubject';
import { TEXT_MODES, type ColumnFilterSpec } from '../lib/columnFilter';
import { severityLabel, stateLabel } from '../lib/format';
import type { TFunction } from 'i18next';
import type { NodeState, Severity } from '../types/api';

/**
 * The fields the filters read.
 *
 * Structural rather than `Alert` so a test can build a row without the whole stream frame.
 */
export interface FilterableAlert extends HasSubject {
  severity: Severity;
  state: NodeState;
  metric?: string | null;
}

/** Resolve a node id to its display name; the raw id when it has not resolved yet. */
export type NameOf = (id: string) => string;

/**
 * The screen's filters, keyed by column — and the keys are the URL keys, the same ones Alerts ▸
 * History uses (`severity` / `node_q` / `metric` / `state`).
 *
 * ⚠️ **`nameOf` is read only by the node column's `readText`.** That resolver is `useEntityNames`'
 * lazy one: asking it about an id *enqueues* that id for the next batch request. `buildPredicate`
 * compiles a column that is not narrowing to `null` and never reads its row, so with the node
 * filter empty nothing is asked. (Sorting by node does ask, for every row — that is the price of
 * putting the list in name order, paid only when someone asks for it.)
 *
 * The node text matches a node's name **or** its raw UUID, or a non-node subject's own name. The
 * raw id is included because it is the handle in a deep link and in an API error, so pasting one
 * has to find its row.
 */
export function activeAlertFilters(
  t: TFunction,
  nameOf: NameOf,
  severities: readonly Severity[],
  states: readonly NodeState[],
): Record<string, ColumnFilterSpec<FilterableAlert>> {
  return {
    severity: {
      kind: 'enum',
      options: severities.map((s) => ({ value: s, label: severityLabel(s) })),
      readValue: (a) => a.severity,
      allLabel: t('history.filter.allSeverities'),
      counts: 'client',
    },
    node_q: {
      kind: 'text',
      // Both modes and NOT: the whole set is in the browser, so a regex costs a pass over a few
      // thousand rows rather than a query, and "everything except the lab switches" is the triage
      // question this screen exists for.
      modes: TEXT_MODES,
      not: true,
      readText: (a) => {
        const subject = alertSubject(a);
        return subject.kind === 'node' ? [nameOf(subject.nodeId), subject.nodeId] : [subjectText(subject)];
      },
      containsSemantics: 'substring',
    },
    metric: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      // The metric name, as History filters it. A liveness alert stores none and so never matches a
      // term — the same rule as History, so one term means the same thing on both screens.
      readText: (a) => [a.metric],
      containsSemantics: 'substring',
    },
    state: {
      kind: 'enum',
      options: states.map((s) => ({ value: s, label: stateLabel(s) })),
      readValue: (a) => a.state,
      allLabel: t('history.filter.allStates'),
      counts: 'client',
    },
  };
}

/** The scope the URL names: a node id, a folder id, or neither (both empty strings). */
export interface AlertScope {
  nodeId: string;
  groupId: string;
}

/** A node's folder: the id, `null` when ungrouped, `undefined` while it has not resolved. */
export type GroupOf = (nodeId: string) => string | null | undefined;

/**
 * The scope predicate, or `null` when nothing is scoped (so the caller skips the pass).
 *
 * - **A node** keeps that node's alerts. A pool's or an organization's alert belongs to no node and
 *   is left out, as History leaves it out.
 * - **A folder** keeps the alerts of nodes filed in it or in any folder beneath it (`subtree`, the
 *   folder's own id included). ⚠️ A node whose folder has not resolved yet is left out *for now*:
 *   asking `groupOf` enqueues it, and the row appears when the answer lands. Showing it meanwhile
 *   would put another site's alerts on screen for a moment, which is the worse of the two errors on
 *   a screen two people may be reading during an incident.
 */
export function scopePredicate(
  scope: AlertScope,
  subtree: ReadonlySet<string>,
  groupOf: GroupOf,
): ((a: HasSubject) => boolean) | null {
  if (scope.nodeId) return (a) => subjectNodeId(a) === scope.nodeId;
  if (scope.groupId) {
    return (a) => {
      const node = subjectNodeId(a);
      if (node === null) return false;
      const group = groupOf(node);
      return group != null && subtree.has(group);
    };
  }
  return null;
}
