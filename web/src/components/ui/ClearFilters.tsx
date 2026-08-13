// SPDX-License-Identifier: AGPL-3.0-only
// "Clear all filters" for the column filter row (ADR-053 Inc.2e).
//
// **It lives in the action row, not in the filter row**, and that is a constraint rather than a
// preference: `.dt-filters` renders exactly one child per column and shares ONE grid template with
// `.dt-head` and every `.dt-row`, so a fourth track would slide the filter controls out from under
// their headers — and `.tsx` tests never run, so nothing would catch it (DataTable.tsx says so at
// the template's definition). `TableToolbar`'s documented order already reserves this slot:
// `search → filters → spacer → count → primary action`.
//
// It renders nothing when nothing is narrowing the list, so a screen can place it unconditionally.

import { useTranslation } from 'react-i18next';
import { activeFilterCount, defaultFilters, type FilterState, type FilterableColumn } from '../../lib/columnFilter';

interface Props<T> {
  columns: readonly FilterableColumn<T>[];
  filters: FilterState;
  onChange: (next: FilterState) => void;
  /** A control outside the filter row that also narrows this list — the Events page's node picker,
   *  say. Counted in the badge and cleared with the rest, because an operator who presses "clear
   *  all filters" and is still looking at a filtered list has been told something untrue. */
  extra?: { active: boolean; clear: () => void };
}

export function ClearFilters<T>({ columns, filters, onChange, extra }: Props<T>) {
  const { t } = useTranslation('common');
  const count = activeFilterCount(columns, filters) + (extra?.active ? 1 : 0);
  if (count === 0) return null;
  return (
    <button
      type="button"
      className="clear-filters"
      onClick={() => {
        onChange(defaultFilters(columns));
        extra?.clear();
      }}
    >
      {t('filter.clearAllCount', { count })}
    </button>
  );
}
