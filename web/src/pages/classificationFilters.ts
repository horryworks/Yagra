// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Classification rules — what each column's filter reads off a row (ADR-053 Inc.5).
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md), and because this is the only
// judgement on that screen: the page is layout, `lib/filterPredicate.ts` is the row test.
//
// Client-side, and permitted to be: the ruleset is bounded by what an operator typed in, not by
// fleet size (`ui-conventions.md`). Every rule is in the browser, so the counts are exact.

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import type { ClassificationRule } from '../types/api';

/**
 * The filter row, keyed by `Column.key`.
 *
 * The screen had one search box over five fields at once — the OID prefix, the sysDescr regex, the
 * maker, the model and the resolved profile name — so it could not say which was meant. `huawei`
 * matched a rule *pointing at* the Huawei profile and a rule *matching* Huawei hardware
 * identically, and those are opposite questions when you are working out why a device classified
 * wrong. Splitting them is the whole point of the filter row.
 *
 * `profileName` resolves an id through the loaded profile list; it is consulted only while that
 * column is narrowing, because `buildPredicate` compiles an inactive column to `null`.
 */
export function classificationRuleFilters(
  t: TFunction,
  profileName: (id: string) => string,
): Record<string, ColumnFilterSpec<ClassificationRule>> {
  return {
    // Both matchers under one column because the column renders both, and a rule carries either or
    // both. Reading them as two strings rather than one joined one keeps a term from matching
    // across the gap between them.
    match: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (r) => [r.sysobjectid_prefix ?? '', r.sysdescr_regex ?? '', r.vendor ?? '', r.model ?? ''],
      containsSemantics: 'substring',
      placeholder: t('rules.cols.match'),
    },
    profile: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (r) => [profileName(r.profile_id)],
      containsSemantics: 'substring',
      placeholder: t('rules.cols.profile'),
    },
    status: {
      kind: 'enum',
      options: [
        { value: 'enabled', label: t('rules.enabled') },
        { value: 'disabled', label: t('rules.disabled') },
      ],
      readValue: (r) => (r.enabled ? 'enabled' : 'disabled'),
      allLabel: t('rules.filter.allStatuses'),
      counts: 'client',
    },
  };
}
