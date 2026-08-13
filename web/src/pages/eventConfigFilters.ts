// SPDX-License-Identifier: AGPL-3.0-only
// What each column's filter reads off a row on the three passive-event configuration screens
// (ADR-053 Inc.5): Alerts ▸ Event sources, Alerts ▸ Event rules, and Alerts ▸ Routing.
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md). Client-side, and permitted to be:
// both lists are bounded by what an operator typed in, not by fleet size (`ui-conventions.md`), so
// every row is in the browser and the facet counts are exact.
//
// One module for the two because they share a vocabulary — both have an enabled/disabled column,
// and having each screen decide what "disabled" filters to is how they would end up spelling it
// two ways.

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import type { EventRule, EventSource } from '../types/api';

/**
 * The enabled/disabled column, shared by all three screens.
 *
 * `labels` differs per screen only because the three namespaces spell the two words in their own
 * keys; the *values* are fixed, so a URL taken on one screen means the same thing on another.
 */
function enabledSpec<T extends { enabled: boolean }>(
  labels: { on: string; off: string; all: string },
): ColumnFilterSpec<T> {
  return {
    kind: 'enum',
    options: [
      { value: 'enabled', label: labels.on },
      { value: 'disabled', label: labels.off },
    ],
    readValue: (r) => (r.enabled ? 'enabled' : 'disabled'),
    allLabel: labels.all,
    counts: 'client',
  };
}

/** Alerts ▸ Event sources. */
export function eventSourceFilters(
  t: TFunction,
  kinds: readonly string[],
): Record<string, ColumnFilterSpec<EventSource>> {
  return {
    name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (r) => [r.name],
      containsSemantics: 'substring',
      placeholder: t('eventSources.cols.name'),
    },
    // The kinds come from the rows themselves rather than a hardcoded list: `EventSource.kind` is a
    // bare string on the wire, so a source created by a newer core is a value this build has never
    // heard of — and one that must still be selectable rather than silently absent from the list.
    kind: {
      kind: 'enum',
      options: kinds.map((k) => ({ value: k, label: k })),
      readValue: (r) => r.kind,
      allLabel: t('eventSources.filter.allKinds'),
      counts: 'client',
    },
    status: enabledSpec({
      on: t('status.enabled'),
      off: t('status.disabled'),
      all: t('eventSources.filter.allStatuses'),
    }),
  };
}

/** Alerts ▸ Event rules. */
export function eventRuleFilters(
  t: TFunction,
  severities: readonly string[],
  scopeLabel: (r: EventRule) => string,
): Record<string, ColumnFilterSpec<EventRule>> {
  return {
    name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (r) => [r.name],
      containsSemantics: 'substring',
      placeholder: t('eventRules.cols.name'),
    },
    severity: {
      kind: 'enum',
      options: severities.map((s) => ({ value: s, label: t(`severity.${s}`) })),
      readValue: (r) => r.severity,
      allLabel: t('eventRules.filter.allSeverities'),
      counts: 'client',
    },
    // The matcher, as text. `regex` is offered because the *value* being searched is itself often
    // a regular expression, and "every rule whose pattern anchors at the start" is a real question
    // when rules stop matching. `clear_pattern` is read too: a rule's clear side is part of what it
    // matches, and a term that finds only the fire half would hide half the answer.
    pattern: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (r) => [r.pattern, r.clear_pattern ?? ''],
      containsSemantics: 'substring',
      placeholder: t('eventRules.cols.pattern'),
    },
    scope: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (r) => [scopeLabel(r)],
      containsSemantics: 'substring',
      placeholder: t('eventRules.cols.scope'),
    },
    status: enabledSpec({
      on: t('status.enabled'),
      off: t('status.disabled'),
      all: t('eventRules.filter.allStatuses'),
    }),
  };
}

// Alerts ▸ Routing is **not** here: its two tables already had a spec module of their own
// (`routingFilters.ts`), and one of its columns carries a domain rule the shared `enabledSpec`
// below cannot express — a routing rule with no severity routes every severity, so it must match
// whichever one is selected rather than be hidden by it.
