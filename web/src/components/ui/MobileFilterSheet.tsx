// SPDX-License-Identifier: AGPL-3.0-only
// Every filter of a list, stacked: the phone's bottom sheet (ADR-053 decision 14), and since
// ADR-228 the desktop popover of a list that has no header row to hang a funnel on.
//
// `DataTable` renders cards on mobile, and a card list has no header row - so there is nowhere to
// hang a column's funnel. The action row gets a `Filter (N)` button instead, and it opens the shared
// `Modal`, which `html[data-viewport='mobile']` already turns into a bottom sheet. A desktop list
// drawn without a header (Active alerts, Users, Runs) is in the same position, and gets the same
// sections in an `AnchoredPopover`.
//
// Two reuses, each load-bearing:
//   - **`Modal`** / **`AnchoredPopover`**, so focus trapping, the safe-area inset, placement and
//     the close behaviour are inherited rather than re-typed.
//   - **`FilterBody`**, the same component a header's funnel opens. A second editor would drift,
//     and mobile is where a drift goes unnoticed for a release or two.
//
// The `.mfilt-*` CSS prefix names the styles this file owns; `.mfilt-btn` is how
// `filterGeometry.spec.ts` finds the button.

import { useCallback, useRef, useState } from 'react';
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
import { AnchoredPopover } from './AnchoredPopover';
import { focusPopoverTrigger } from './focusPopoverTrigger';
import { FunnelIcon } from './icons';
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
      <FilterSections
        columns={columns}
        labels={labels}
        filters={filters}
        onChange={onChange}
        counts={counts}
      />
    </Modal>
  );
}

interface SectionsProps<T> {
  columns: readonly FilterableColumn<T>[];
  labels: Record<string, string>;
  filters: FilterState;
  onChange: (next: FilterState) => void;
  counts?: Record<string, Record<string, number>>;
}

/** One section per column: its name (with a dot while it narrows the list) above its editor. */
export function FilterSections<T>({
  columns,
  labels,
  filters,
  onChange,
  counts,
}: SectionsProps<T>) {
  return (
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
  );
}

/**
 * The action-row control, decided here rather than at ~40 call sites (ADR-228).
 *
 *  - On a phone it opens the sheet, because a card list has no header.
 *  - On a desktop it exists only for a list with no header row (`menu`), and opens the same
 *    sections in a popover. A list with a header filters from each column's funnel, so it draws
 *    no button at all - the press that added a row to the table is gone.
 */
export function FilterButton<T>({
  columns,
  filters,
  baseline,
  onOpen,
  menu,
}: {
  columns: readonly FilterableColumn<T>[];
  filters: FilterState;
  /** The state that counts as "nothing set", for a table whose own default narrows. See
   *  `activeFilterCount`. Pass the same object to `ClearFilters`, or the two disagree. */
  baseline?: FilterState;
  /** Opens the bottom sheet (phone). */
  onOpen: () => void;
  /** The desktop popover, for a list drawn without a header. Absent: no desktop button. */
  menu?: SectionsProps<T>;
}) {
  const { t } = useTranslation('common');
  const mode = useViewportMode();
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLSpanElement>(null);
  const dismiss = useCallback((restoreFocus: boolean) => {
    setOpen(false);
    if (restoreFocus) focusPopoverTrigger(wrapRef.current, 'dialog');
  }, []);
  const n = activeFilterCount(columns, filters, baseline);
  const label = n > 0 ? t('filter.buttonCount', { count: n }) : t('filter.button');

  if (mode === 'mobile') {
    return (
      <button type="button" className={n > 0 ? 'mfilt-btn on' : 'mfilt-btn'} onClick={onOpen}>
        {label}
      </button>
    );
  }
  if (!menu) return null;
  return (
    <span className="mfilt-menu" ref={wrapRef}>
      <button
        type="button"
        className={n > 0 || open ? 'mfilt-btn icon on' : 'mfilt-btn icon'}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={label}
        onClick={() => setOpen((o) => !o)}
      >
        <FunnelIcon width={14} height={14} />
        {n > 0 && <span className="mfilt-n">{n}</span>}
      </button>
      <AnchoredPopover
        open={open}
        anchorRef={wrapRef}
        role="dialog"
        label={t('filter.sheet')}
        align="start"
        className="mfilt-pop"
        onDismiss={dismiss}
      >
        <FilterSections {...menu} />
      </AnchoredPopover>
    </span>
  );
}
