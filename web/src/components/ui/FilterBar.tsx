// SPDX-License-Identifier: AGPL-3.0-only
// The column filter row for a list that has no columns (ADR-053 Inc.6, decision E).
//
// Four screens are lists without a header row: Alerts ▸ Active (`AlertRows` is a run of spans),
// Troubleshoot ▸ Runs, Settings ▸ Users (an identity list) and the node tree. A filter row cannot be
// put under headers that do not exist, so those screens get this instead: the **same**
// `ColumnFilterCell`s, laid out horizontally with their names beside them.
//
// ⚠️ **Do not reach for `.dt-filters` here.** That grid renders one child per column and shares a
// single `gridTemplateColumns` string with `.dt-head` and every `.dt-row` (DataTable.tsx says so at
// the template's definition), so borrowing it for a list with no columns puts three grids out of
// step and nothing catches it — `.tsx` tests never run. This is a plain flex row that knows nothing
// about `DataTable`.
//
// What the screens actually gain is **multi-select**, not a look: `FilterSelect` was single-valued,
// so "critical and warning" and "operator and admin" were unsayable. The filter state, the URL
// codec, `buildPredicate`, `ClearFilters` and the mobile sheet are all the same ones the filter row
// uses, so a screen converted to this is a screen that gained NOT, regex and set values too.

import { useTranslation } from 'react-i18next';
import { ColumnFilterCell } from './ColumnFilterCell';
import type { FilterState, FilterableColumn } from '../../lib/columnFilter';
import { useFilterRowVisible } from './MobileFilterSheet';
import './FilterBar.css';

interface Props<T> {
  columns: readonly FilterableColumn<T>[];
  /** Plain-text names, keyed by column key. Rendered beside each control, because there is no header
   *  above it to name it — the one thing this cannot borrow from the filter row. */
  labels: Record<string, string>;
  filters: FilterState;
  onChange: (next: FilterState) => void;
  /** Facet counts per key, when the screen has them. Client-side lists compute them with
   *  `lib/filterCounts.ts`; a server-side one fetches on `onFilterOpen`. */
  counts?: Record<string, Record<string, number>>;
  onFilterOpen?: (columnKey: string) => void;
}

/**
 * The bar. **Renders nothing on mobile, and nothing while the desktop toggle is closed** — both
 * gates live here rather than at each call site.
 *
 * `useFilterRowVisible` is the other half of `FilterButton`'s either/or, and the two are declared
 * in the same file. Leaving the choice to the caller is exactly how the filter row shipped showing
 * `Filter (N)` *and* the row underneath — two controls editing one state — so the rule is that the
 * two halves of one decision stay in one language.
 */
export function FilterBar<T>({
  columns,
  labels,
  filters,
  onChange,
  counts,
  onFilterOpen,
}: Props<T>) {
  const { t } = useTranslation('common');
  const visible = useFilterRowVisible(columns, filters);
  if (!visible || columns.length === 0) return null;
  return (
    <div className="fbar" role="group" aria-label={t('filter.bar')}>
      {columns.map((c) => {
        const label = labels[c.key] ?? c.key;
        return (
          <div className="fbar-item" key={c.key}>
            <span className="fbar-label">{label}</span>
            <ColumnFilterCell
              spec={c.filter}
              value={filters[c.key] ?? ''}
              onChange={(next) => onChange({ ...filters, [c.key]: next })}
              counts={counts?.[c.key]}
              label={label}
              onOpen={onFilterOpen ? () => onFilterOpen(c.key) : undefined}
            />
          </div>
        );
      })}
    </div>
  );
}
