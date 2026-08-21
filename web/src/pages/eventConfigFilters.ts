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
import { severityLabel } from '../lib/format';
import type { EventRule, EventSource, Severity } from '../types/api';

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
    // The kinds come from the rows themselves rather than a hardcoded list, so a source created by
    // a newer core stays selectable rather than being silently absent from the filter.
    //
    // ⚠️ This used to say "`EventSource.kind` is a bare string on the wire". It is not — the
    // generated contract types it as a union, and the compiler rejected a test row that said
    // otherwise. The *behaviour* the comment described is still the reason for reading the rows: a
    // TypeScript union is a compile-time claim, and a core one version ahead puts its new token on
    // the wire regardless. Reading the rows is right; the stated reason was wrong.
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
  severities: readonly Severity[],
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
    // `severityLabel`, not a key built here: this module is called with `t` bound to
    // `alertsConfig`, whose top level has no `severity` block, so the dropdown rendered the raw
    // key `severity.critical` beside a badge in the same column reading `Critical`. EN/JA parity
    // cannot catch that — the key is missing from both locales equally. The other three severity
    // filters (active alerts, history, routing) already resolve it through `format:`.
    severity: {
      kind: 'enum',
      options: severities.map((s) => ({ value: s, label: severityLabel(s) })),
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
