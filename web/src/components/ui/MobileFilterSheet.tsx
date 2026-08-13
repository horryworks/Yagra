// SPDX-License-Identifier: AGPL-3.0-only
// The mobile answer to the column filter row (ADR-053 decision 14).
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

/** The action-row button that opens the sheet. Separate so a screen can place it in its own toolbar
 *  without pulling the sheet's state up before it is needed.
 *
 * **It renders nothing on desktop, and that gate lives here rather than at the call site.**
 * `DataTable` shows the filter row exactly when it is *not* in card mode, off this same
 * `useViewportMode()`; the button is the other half of that either/or, so the two must not be
 * decided in two places. Shipped without this, every desktop screen showed `Filter (N)` in the
 * action row *and* the filter row underneath — two controls editing one state, which is the
 * coexistence this ADR set out to remove. A CSS `display:none` would have fixed the symptom while
 * leaving the decision in a second language, and 29 more screens are still to adopt this pattern.
 */
export function MobileFilterButton<T>({
  columns,
  filters,
  onOpen,
}: {
  columns: readonly FilterableColumn<T>[];
  filters: FilterState;
  onOpen: () => void;
}) {
  const { t } = useTranslation('common');
  const mode = useViewportMode();
  const n = activeFilterCount(columns, filters);
  if (mode !== 'mobile') return null;
  return (
    <button type="button" className={n > 0 ? 'mfilt-btn on' : 'mfilt-btn'} onClick={onOpen}>
      {n > 0 ? t('filter.sheetButtonCount', { count: n }) : t('filter.sheetButton')}
    </button>
  );
}
