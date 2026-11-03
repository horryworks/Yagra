// SPDX-License-Identifier: AGPL-3.0-only
// A column's filter, opened from a funnel beside its header (ADR-228).
//
// The same popover body as the filter row's cell (`FilterBody`), behind a different trigger: the
// row spent a band of the table on controls most columns never use, and opened only after a press
// on a toolbar button. The funnel needs no band and no toggle. What the closed row trigger used to
// say — which columns are narrowing the list — is said by `FilterChips` above the table instead,
// so the funnel only has to say *that* its column is filtered (accent colour and a dot).

import { useCallback, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { AnchoredPopover } from './AnchoredPopover';
import { FilterBody } from './ColumnFilterCell';
import { focusPopoverTrigger } from './focusPopoverTrigger';
import { FunnelIcon } from './icons';
import type { ColumnFilterSpec, FilterState, FilterableColumn } from '../../lib/columnFilter';
import { summaryIsActive } from '../../lib/filterSummary';
import { useViewportMode } from '../../lib/viewport';
import type { SortDir } from '../../lib/tableSort';
import './HeaderFilter.css';

interface Props<T> {
  spec: ColumnFilterSpec<T>;
  value: string;
  onChange: (next: string) => void;
  counts?: Record<string, number>;
  /** Plain-text column name, for the funnel's accessible name. */
  label: string;
  /** Open toward the start edge (left columns) or the end edge (right columns). */
  align?: 'start' | 'end';
  /** Called as the popover opens: a server-counted list fetches this column's counts here. */
  onOpen?: () => void;
}

export function HeaderFilter<T>({
  spec,
  value,
  onChange,
  counts,
  label,
  align = 'start',
  onOpen,
}: Props<T>) {
  const { t } = useTranslation('common');
  const [open, setOpen] = useState(false);
  // See `ColumnFilterCell`: focus can move into the panel only once it is placed and visible.
  const [placed, setPlaced] = useState(false);
  const wrapRef = useRef<HTMLSpanElement>(null);
  const dismiss = useCallback((restoreFocus: boolean) => {
    setOpen(false);
    if (restoreFocus) focusPopoverTrigger(wrapRef.current, 'dialog');
  }, []);
  // A phone filters from the sheet (`FilterButton`). A hand-built table keeps its header in the
  // DOM there, so the funnel says no for itself rather than trusting every table's CSS.
  const mobile = useViewportMode() === 'mobile';
  const active = summaryIsActive(spec, value);
  if (mobile) return null;
  const cls = `hf-trigger${active ? ' on' : ''}${open ? ' open' : ''}`;
  return (
    <span className="hf" ref={wrapRef}>
      <button
        type="button"
        className={cls}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={t('filter.open', { column: label })}
        // The header's own click sorts; this one must not reach it.
        onClick={(e) => {
          e.stopPropagation();
          const next = !open;
          setOpen(next);
          if (next) onOpen?.();
        }}
      >
        <FunnelIcon width={12} height={12} />
      </button>
      <AnchoredPopover
        open={open}
        anchorRef={wrapRef}
        role="dialog"
        label={t('filter.open', { column: label })}
        align={align}
        onDismiss={dismiss}
        onPlacedChange={setPlaced}
      >
        <FilterBody
          spec={spec}
          value={value}
          onChange={onChange}
          counts={counts}
          label={label}
          takeFocus={placed}
        />
      </AnchoredPopover>
    </span>
  );
}

/** What a hand-built table hands each of its header cells: the list's filter state, once. */
export interface HeaderFilterBinding<T> {
  columns: readonly FilterableColumn<T>[];
  filters: FilterState;
  onChange: (next: FilterState) => void;
  counts?: Record<string, Record<string, number>>;
  /** Plain-text column names, keyed by column key. */
  labels: Record<string, string>;
}

/**
 * A header cell of a table that is not a `DataTable` (Interfaces, Collection, Discovery, Device
 * profiles): the label, and the column's funnel when `colKey` names a filterable column. A key
 * with no filter draws the label alone, so every cell of a header can go through this.
 *
 * The caller keeps its own class and grid placement — the template lives with the header that
 * must agree with it (ADR-129).
 */
export function FilterHeaderCell<T>({
  bind,
  colKey,
  className,
  style,
  align,
  children,
}: {
  bind: HeaderFilterBinding<T>;
  colKey: string;
  className: string;
  style?: CSSProperties;
  align?: 'right';
  children: ReactNode;
}) {
  const col = bind.columns.find((c) => c.key === colKey);
  if (!col) {
    return (
      <div className={className} style={style}>
        {children}
      </div>
    );
  }
  return (
    <div className={`${className} hcell${align === 'right' ? ' right' : ''}`} style={style}>
      {/* The title carries the whole name: the funnel takes room from a narrow column's label. */}
      <span className="hcell-label" title={bind.labels[colKey]}>
        {children}
      </span>
      <HeaderFilter
        spec={col.filter}
        value={bind.filters[colKey] ?? ''}
        onChange={(next) => bind.onChange({ ...bind.filters, [colKey]: next })}
        counts={bind.counts?.[colKey]}
        label={bind.labels[colKey] ?? colKey}
        align={align === 'right' ? 'end' : 'start'}
      />
    </div>
  );
}

/**
 * A header label that sorts its column. Each press goes ascending → descending → no sort, and only
 * the column the list is ordered by carries an arrow (ADR-228 Inc.3, as the mock had it). A real
 * button, so the sort is keyboard-operable. The arrow appears on the press, never on hover, so the
 * label does not move under a pointer that is only passing.
 */
export function SortHeaderButton({
  active,
  onClick,
  children,
}: {
  /** The direction when this column is the sort, `null` otherwise. */
  active: SortDir | null;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      className={active ? 'dt-h-sort active' : 'dt-h-sort'}
      onClick={onClick}
      aria-sort={active === null ? 'none' : active === 'asc' ? 'ascending' : 'descending'}
    >
      {children}
      {/* Drawn by CSS from `data-dir`, so the header's text is its name alone — for a screen
          reader, and for anything that reads the header to find a column. */}
      {active && <span className="dt-h-arrow" data-dir={active} aria-hidden="true" />}
    </button>
  );
}
