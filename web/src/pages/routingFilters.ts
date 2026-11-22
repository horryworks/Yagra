// SPDX-License-Identifier: AGPL-3.0-only
// What each column's filter reads off a row on the two tables of Alerts ▸ Routing — notification
// channels, and the rules that point at them.
//
// Client-side: both lists are bounded by what an operator configured, not by fleet size
// (ui-conventions, "scale-aware lists"). In a `.ts` so a test can reach the judgement — Vitest
// never executes a `.tsx` (testing.md).
//
// The row predicate itself is `lib/filterPredicate.ts::buildPredicate`, shared by every screen
// (ADR-053 Inc.5); the two hand-written `matchesX(row, filters)` functions this file used to hold
// are gone. What stayed is the one thing that is genuinely per-column, plus the one rule below that
// is genuinely per-*domain*.

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import { channelKindLabel } from '../lib/channelKinds';
import {
  SEVERITIES,
  type NotificationChannel,
  type RoutingRule,
  type Severity,
} from '../types/api';

/** URL-key prefixes for this route's two tables (ADR-153 decision 3). Both have a `name` and a
 *  `status` column, so unprefixed they would filter each other; the ledger in
 *  `filterSpecRegistry.test.ts` checks the pair is disjoint. */
export const CHANNEL_FILTER_PREFIX = 'channels.';
export const ROUTING_RULE_FILTER_PREFIX = 'rules.';

/** Whether the rules can be put in order by dragging (ADR-262 decision 14, ADR-255): only while
 *  every rule is on screen in its stored order - not narrowed by a filter, not sorted by a column.
 *  `sortBy` is `''` when no column is sorted. */
export function canReorderRules(canSystem: boolean, sortBy: string, filtered: boolean): boolean {
  return canSystem && sortBy === '' && !filtered;
}

/** The rules in the order just dropped, until the re-read that shows it arrives. A rule the drop
 *  does not name (created meanwhile) keeps its place after them. */
export function inDroppedOrder<T extends { id: string }>(
  rows: readonly T[],
  dropped: readonly string[] | null,
): T[] {
  if (!dropped) return [...rows];
  const at = new Map(dropped.map((id, i) => [id, i]));
  return [...rows].sort(
    (a, b) => (at.get(a.id) ?? Number.MAX_SAFE_INTEGER) - (at.get(b.id) ?? Number.MAX_SAFE_INTEGER),
  );
}

/** Alerts ▸ Routing, the channels table. Keyed by `Column.key`. */
export function channelFilters(
  t: TFunction,
  kinds: readonly string[],
): Record<string, ColumnFilterSpec<NotificationChannel>> {
  return {
    name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (c) => [c.name],
      containsSemantics: 'substring',
      placeholder: t('routing.channels.cols.name'),
    },
    kind: {
      kind: 'enum',
      // Options come from the rows (so the list only offers kinds that are actually present), but
      // the *labels* come from the same map the add-channel dialog uses — this column used to read
      // `pagerduty` under a dialog that read `PagerDuty`.
      options: kinds.map((k) => ({ value: k, label: channelKindLabel(k) })),
      readValue: (c) => c.kind,
      allLabel: t('routing.channels.allKinds'),
      counts: 'client',
    },
    status: {
      kind: 'enum',
      options: [
        { value: 'enabled', label: t('common:filter.enabled') },
        { value: 'disabled', label: t('common:filter.disabled') },
      ],
      readValue: (c) => (c.enabled ? 'enabled' : 'disabled'),
      allLabel: t('common:filter.allEnabled'),
      counts: 'client',
    },
  };
}

/** Alerts ▸ Routing, the rules table. Keyed by `Column.key`. */
export function routingRuleFilters(
  t: TFunction,
  severityLabel: (s: Severity) => string,
  folderPath: (id: string) => string = (id) => id,
): Record<string, ColumnFilterSpec<RoutingRule>> {
  return {
    name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (r) => [r.name],
      containsSemantics: 'substring',
      placeholder: t('routing.rules.cols.name'),
    },
    // ⚠️ **A rule with no severity of its own routes *every* severity, so it must match whichever
    // severity is selected rather than be hidden by it.** The question an operator asks here is
    // "what would page me for a critical", and answering it while omitting the rules that always
    // page would be worse than not offering the filter. `readValue` returning an array is the
    // mechanism — a row may legitimately carry several values, and this row carries all of them.
    severity: {
      kind: 'enum',
      options: SEVERITIES.map((s) => ({ value: s, label: severityLabel(s) })),
      readValue: (r) => r.severity ?? SEVERITIES,
      allLabel: t('routing.rules.allSeverities'),
      counts: 'client',
    },
    // A rule naming nothing reaches every node but reads as having nothing here, so "TeamA" finds
    // the rules written for that tag or folder rather than every rule that would also page for it
    // (ADR-259, ADR-262). Folders are read by their path, as the column shows them.
    target: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (r) => [...(r.group_ids ?? []).map(folderPath), ...r.tags],
      containsSemantics: 'substring',
      placeholder: t('appliesTo.title'),
    },
    status: {
      kind: 'enum',
      options: [
        { value: 'enabled', label: t('common:filter.enabled') },
        { value: 'disabled', label: t('common:filter.disabled') },
      ],
      readValue: (r) => (r.enabled ? 'enabled' : 'disabled'),
      allLabel: t('common:filter.allEnabled'),
      counts: 'client',
    },
  };
}
