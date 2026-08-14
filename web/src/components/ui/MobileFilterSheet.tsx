// SPDX-License-Identifier: AGPL-3.0-only
// The mobile answer to the column filter row (ADR-053 decision 14), plus — since Inc.9 — the
// control that opens and closes that row on a desktop.
//
// `DataTable` renders cards on mobile, and a card list has no header row — so there is nowhere to
// hang a filter row. The action row gets a `Filter (N)` button instead, and it opens the shared
// `Modal`, which `html[data-viewport='mobile']` already turns into a bottom sheet.
//
// Two reuses, each load-bearing:
//   - **`Modal`**, so focus trapping, the safe-area inset and the close behaviour are inherited
//     rather than re-typed. Every one of those was got wrong at least once before `Modal` existed.
//   - **`FilterBody`**, the same component the desktop popover renders. A second mobile-only editor
//     would drift, and mobile is where a drift goes unnoticed for a release or two.
//
// ⚠️ `FilterButton` is no longer mobile-only, which is why it is no longer called
// `MobileFilterButton`. The `.mfilt-*` CSS prefix is deliberately left alone: it names the styles
// this file owns, and renaming it would churn `MobileFilterSheet.css` and the Tier1 phone check
// (`.mfilt-btn` is how `filterGeometry.spec.ts` asserts the sheet half of the either/or) for
// nothing this change needs.

import { useTranslation } from 'react-i18next';
import { Modal } from './Modal';
import { FilterBody } from './ColumnFilterCell';
import {
  activeFilterCount,
  defaultFilters,
  type FilterState,
  type FilterableColumn,
} from '../../lib/columnFilter';
import { summaryIsActive } from '../../lib/filterSummary';
import { filterRowForced, filterRowVisible } from '../../lib/filterRow';
import { usePrefsStore } from '../../prefs';
import { useViewportMode } from '../../lib/viewport';
import './MobileFilterSheet.css';

interface Props<T> {
  columns: readonly FilterableColumn<T>[];
  /** Plain-text column names, keyed by column key — the sheet has room for real labels, unlike a
   *  90px grid track, so it shows the column name above each control. */
  labels: Record<string, string>;
  filters: FilterState;
  onChange: (next: FilterState) => void;
  counts?: Record<string, Record<string, number>>;
  onClose: () => void;
}

export function MobileFilterSheet<T>({
  columns,
  labels,
  filters,
  onChange,
  counts,
  onClose,
}: Props<T>) {
  const { t } = useTranslation('common');
  const active = activeFilterCount(columns, filters);

  return (
    <Modal
      title={t('filter.sheet')}
      onClose={onClose}
      footer={
        <button
          type="button"
          className="mfilt-clear"
          disabled={active === 0}
          onClick={() => onChange(defaultFilters(columns))}
        >
          {t('filter.clearAll')}
        </button>
      }
    >
      <div className="mfilt">
        {columns.map((c) => {
          const value = filters[c.key] ?? '';
          const label = labels[c.key] ?? c.key;
          return (
            <section key={c.key} className="mfilt-sec">
              <h3 className="mfilt-h">
                {label}
                {summaryIsActive(c.filter, value) && (
                  <span className="mfilt-on" aria-hidden="true">
                    ●
                  </span>
                )}
              </h3>
              <FilterBody
                spec={c.filter}
                value={value}
                onChange={(next) => onChange({ ...filters, [c.key]: next })}
                counts={counts?.[c.key]}
                label={label}
              />
            </section>
          );
        })}
      </div>
    </Modal>
  );
}

/**
 * The action-row control. **One button, two jobs, decided here rather than at ~40 call sites.**
 *
 *  - On a phone it opens the sheet, because a card list has no header to hang a row under.
 *  - On a desktop it opens and closes the filter row (ADR-053 Inc.9). Before Inc.9 it rendered
 *    nothing here at all, and the row was drawn unconditionally.
 *
 * The two are the same either/or they have always been, now with three states instead of two, and
 * `useFilterRowVisible` below is still the other half. Shipping the desktop half at the call sites
 * would repeat the failure decision 14 already paid for: `Filter (N)` and the row visible at once,
 * two controls editing one state.
 *
 * ⚠️ **While a filter is narrowing the list the button cannot close the row** (`filterRowForced`).
 * `aria-disabled` rather than `disabled`, so it stays focusable and its `title` is reachable from
 * the keyboard — and the way out is the `ClearFilters` button that is guaranteed to be beside it,
 * since both render off the same non-zero count.
 */
export function FilterButton<T>({
  columns,
  filters,
  baseline,
  onOpen,
}: {
  columns: readonly FilterableColumn<T>[];
  filters: FilterState;
  /** The state that counts as "nothing set", for a table whose own default narrows. See
   *  `activeFilterCount`. Pass the same object to `useFilterRowVisible` and `ClearFilters`, or the
   *  three will disagree about whether this list is filtered. */
  baseline?: FilterState;
  /** Opens the bottom sheet. Mobile-only — the desktop press toggles the row instead. */
  onOpen: () => void;
}) {
  const { t } = useTranslation('common');
  const mode = useViewportMode();
  const open = usePrefsStore((s) => s.filterRowOpen);
  const toggle = usePrefsStore((s) => s.toggleFilterRow);
  const n = activeFilterCount(columns, filters, baseline);
  const label = n > 0 ? t('filter.buttonCount', { count: n }) : t('filter.button');

  if (mode === 'mobile') {
    return (
      <button type="button" className={n > 0 ? 'mfilt-btn on' : 'mfilt-btn'} onClick={onOpen}>
        {label}
      </button>
    );
  }

  const locked = filterRowForced(n);
  const shown = filterRowVisible(mode, open, n);
  return (
    <button
      type="button"
      className={shown ? 'mfilt-btn on' : 'mfilt-btn'}
      // Pressed, not checked: this reveals a region rather than selecting a value. Set only on
      // desktop — the mobile press opens a dialog, which is not a toggle.
      aria-pressed={shown}
      aria-disabled={locked || undefined}
      title={locked ? t('filter.lockedHint') : undefined}
      onClick={locked ? undefined : toggle}
    >
      {label}
    </button>
  );
}

/**
 * The other half of that either/or: **a filter row or bar draws exactly when this says so.**
 * Every surface that renders `ColumnFilterCell`s in a row asks this before rendering them.
 *
 * It lives here, twenty lines from the button, because the two are one decision — the same reason
 * the button's own gate is not at its call sites. It was written out separately five times instead:
 * `DataTable` and `FilterBar` each had their own `useViewportMode()` call (correct, and no longer
 * duplicated), a `display: none` in `styles/table.css` covered one hand-rolled row, and the other
 * four hand-rolled rows had nothing at all. Those four kept drawing a desktop grid on a phone —
 * `Interfaces` stuck its row halfway down the scroller, at the height of a header mobile hides, and
 * the interface rows scrolled through the gap above it.
 *
 * ⚠️ **Since Inc.10 the two surfaces that lay cells out call it for you** — `FilterBar`, and
 * `ColumnFilterRow`, which every grid row now goes through. A screen calls this directly only if it
 * renders `ColumnFilterCell`s in some third arrangement, and there is none today.
 *
 * A hook rather than a wrapper component on purpose: these rows *are* CSS grids that share a
 * `gridTemplateColumns` with their header, so an element wrapped around one breaks the alignment
 * the row exists to keep. That is why `ColumnFilterRow` **is** the grid element rather than
 * something around it, and why the template stays with the caller that also owns the header.
 *
 * ⚠️ **It takes the columns and the current values** (Inc.9) so it can answer "is anything
 * narrowing this list", which forces the row open. Passing a count in from each caller would put
 * that arithmetic back in seven places; the rule itself is `lib/filterRow.ts`, where a test runs.
 */
export function useFilterRowVisible<T>(
  columns: readonly FilterableColumn<T>[],
  filters: FilterState,
  /** See `FilterButton.baseline` — the two must be given the same object. */
  baseline?: FilterState,
): boolean {
  const mode = useViewportMode();
  const open = usePrefsStore((s) => s.filterRowOpen);
  return filterRowVisible(mode, open, activeFilterCount(columns, filters, baseline));
}
