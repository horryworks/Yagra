// SPDX-License-Identifier: AGPL-3.0-only
// The column filter row as one component (ADR-053 Inc.10 決定 Z).
//
// Seven surfaces lay `ColumnFilterCell`s out in a grid under a header. Two were shared —
// `DataTable`'s `.dt-filters` and `FilterBar` (which is a flex row, not a grid, and stays separate
// for the reason its own header gives). The other five were hand-written, and each had rewritten the
// same four things: the visibility gate, the `role="group"` + `aria-label`, a local `cell(key)`
// helper doing an untyped `columns.find`, and `<div className="dt-f empty" />` for every track with
// no control. Four copies of a rule is where a rule goes wrong, and this row's failure mode is the
// quiet one — a control that sits under the wrong header still looks like a filter row.
//
// **What this component is NOT.** It is not a wrapper *around* a filter row. It **is** the grid
// element: the caller's `className` and its `gridTemplateColumns` land on this component's own root,
// so the row stays a direct sibling of the header sharing one template. `MobileFilterSheet.tsx`
// warns against wrapping one of these rows in an element, and that warning still stands — an extra
// box between the grid and its tracks is exactly what breaks the alignment the row exists to keep.
//
// **What stays with the caller, on purpose:** the grid template itself. `DataTable` derives it from
// its columns and hands the same string to three grids (`.dt-head`, this row, every `.dt-row`); the
// hand-rolled tables declare theirs in CSS beside their header's. Moving it here would separate a
// template from the header it must agree with, which is the ADR-054 misalignment all over again.

import type { CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { ColumnFilterCell } from './ColumnFilterCell';
import { useFilterRowVisible } from './MobileFilterSheet';
import type { FilterState, FilterableColumn } from '../../lib/columnFilter';
import { slotKey } from '../../lib/filterRow';
import { slotAlign } from '../../lib/filterRow';

import type { FilterSlot } from '../../lib/filterRow';
export type { FilterSlot };

interface Props<T> {
  columns: readonly FilterableColumn<T>[];
  /** The tracks, **in the header's order**, one entry per grid column. See [`FilterSlot`]. */
  slots: readonly FilterSlot[];
  filters: FilterState;
  onChange: (next: FilterState) => void;
  /** Facet counts per column key, when the screen has them. Client-side lists compute them with
   *  `lib/filterCounts.ts` (⚠️ memoize — it walks every row); a server-side one fetches on
   *  `onFilterOpen`. */
  counts?: Record<string, Record<string, number>>;
  onFilterOpen?: (columnKey: string) => void;
  /** Accessible name for each control. A cell's own trigger reads its *value*, never its column, so
   *  without this a screen reader hears "critical, warning" with nothing saying what of. */
  labels: Record<string, string>;
  /** The row's own CSS class — `.dt-filters`, `.nd-if-filters`, … The caller keeps its stylesheet;
   *  this component owns no CSS of its own. */
  className: string;
  /** `gridTemplateColumns` and the shared `min-width`, for a table that sets them in TS.
   *  A table whose grid lives in CSS passes nothing. */
  style?: CSSProperties;
  /** The state that counts as "nothing set", for a table whose own default narrows. Pass the same
   *  object to `FilterButton` and `ClearFilters`, or the three disagree about whether this list is
   *  filtered — and this one disagreeing means the row is locked open forever. */
  baseline?: FilterState;
}

/**
 * The row. **Renders nothing on mobile, and nothing while the desktop toggle is closed.**
 *
 * That gate is here rather than at the call site for the reason `FilterBar` states: the two halves
 * of `FilterButton`'s either/or have to stay in one language, and five of these rows shipped with no
 * gate at all — drawing a desktop grid on a phone while the button beside them offered the sheet.
 */
export function ColumnFilterRow<T>({
  columns,
  slots,
  filters,
  onChange,
  counts,
  onFilterOpen,
  labels,
  className,
  style,
  baseline,
}: Props<T>) {
  const { t } = useTranslation('common');
  const visible = useFilterRowVisible(columns, filters, baseline);
  if (!visible || columns.length === 0) return null;
  return (
    <div className={className} style={style} role="group" aria-label={t('filter.row')}>
      {slots.map((slot, i) => {
        const key = slotKey(slot);
        const col = key === null ? undefined : columns.find((c) => c.key === key);
        // ⚠️ A named track whose column is missing draws an empty cell rather than nothing, so a
        // renamed key costs one control and not the alignment of every column after it. The lookup
        // is untyped for the same reason `DataTable`'s `specs[c.key]` is — and `.tsx` tests never
        // run, so this fallback is the whole safety net.
        if (!col || key === null) return <div key={`_${i}`} className="dt-f empty" />;
        const label = labels[key] ?? key;
        return (
          <ColumnFilterCell
            key={key}
            spec={col.filter}
            value={filters[key] ?? ''}
            onChange={(next) => onChange({ ...filters, [key]: next })}
            counts={counts?.[key]}
            label={label}
            onOpen={onFilterOpen ? () => onFilterOpen(key) : undefined}
            align={slotAlign(slot)}
          />
        );
      })}
    </div>
  );
}
