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
import { activeFilterCount, type FilterState, type FilterableColumn } from '../../lib/columnFilter';

interface Props<T> {
  columns: readonly FilterableColumn<T>[];
  filters: FilterState;
  /**
   * Reset everything, in **one** state write.
   *
   * ⚠️ This is a single callback rather than the obvious `onChange(defaultFilters(columns))` plus an
   * `extra.clear()`, and the reason is a bug that shipped: on a URL-backed screen both of those
   * build a `URLSearchParams` from the same render's snapshot, React batches the two writes, and the
   * second one restores what the first cleared. The button did nothing at all. Giving the screen one
   * callback is what makes "clear the columns *and* the node picker" expressible as one write.
   */
  onClear: () => void;
  /** Whether a control outside the filter row is also narrowing this list — the Events page's node
   *  picker, say. Counted here, and `onClear` is expected to clear it too: an operator who presses
   *  "clear all filters" and is still looking at a filtered list has been told something untrue. */
  extraActive?: boolean;
}

export function ClearFilters<T>({ columns, filters, onClear, extraActive }: Props<T>) {
  const { t } = useTranslation('common');
  const count = activeFilterCount(columns, filters) + (extraActive ? 1 : 0);
  if (count === 0) return null;
  return (
    <button type="button" className="clear-filters" onClick={onClear}>
      {t('filter.clearAllCount', { count })}
    </button>
  );
}
