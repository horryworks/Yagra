// SPDX-License-Identifier: AGPL-3.0-only
// The words for a filter summary that says something (`lib/filterSummary.ts`), as the toolbar's
// chips print it (`FilterChips`, ADR-228). In a `.ts` so a test can reach it; the filter row's
// closed trigger said the same thing from an inline `switch` in a `.tsx`, where no test ran it.
//
// What it does not say: nothing for `none`, and no mark for a negated text condition — a chip
// prefixes that itself.

import type { TFunction } from 'i18next';
import type { FilterSummary } from './filterSummary';

export type SetSummary = Exclude<FilterSummary, { kind: 'none' }>;

export function filterSummaryText(summary: SetSummary, t: TFunction): string {
  switch (summary.kind) {
    case 'one':
    case 'preset':
      return summary.label;
    case 'many':
      return `${summary.label} ${t('common:filter.more', { count: summary.more })}`;
    case 'text':
      return summary.term;
    case 'number':
      // Three readings, because a one-sided interval read as "3 – " looks unfinished rather than
      // deliberate — and one-sided is the common case ("score 8 or worse").
      if (summary.min !== null && summary.max !== null) return `${summary.min} – ${summary.max}`;
      return summary.min !== null
        ? t('common:filter.numMin', { value: summary.min })
        : t('common:filter.numMax', { value: summary.max });
  }
}
