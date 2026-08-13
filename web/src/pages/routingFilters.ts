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
import {
  SEVERITIES,
  type NotificationChannel,
  type RoutingRule,
  type Severity,
} from '../types/api';

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
      options: kinds.map((k) => ({ value: k, label: k })),
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
