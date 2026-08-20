// SPDX-License-Identifier: AGPL-3.0-only
// High-density, virtualized table (§4: lists assume tens of thousands of rows ⇒ virtual
// scroll + server-side paging; never render the whole set). Generic over the row type. The
// caller fetches pages (keyset) and appends to `rows`; this component windows the DOM with
// @tanstack/react-virtual and calls `onReachEnd` when the user nears the bottom so the
// parent can load the next page.

import { useEffect, useMemo, useRef } from 'react';
import type { ReactNode } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { useTranslation } from 'react-i18next';
import { useViewportMode } from '../../lib/viewport';
import { nextSort, type SortState } from '../../lib/tableSort';
import { ColumnFilterRow } from './ColumnFilterRow';
import { filterableColumns, type ColumnFilterSpec, type FilterState } from '../../lib/columnFilter';
import { minTableWidth } from '../../lib/tableWidth';
import './DataTable.css';

export interface Column<T> {
  key: string;
  header: ReactNode;
  /** Cell renderer. */
  render: (row: T) => ReactNode;
  /** CSS grid track (e.g. '1fr', '120px'). */
  width?: string;
  /** End-align the header + cells (counts, actions). */
  align?: 'right';
  /** Make the header a sort control (design-system §4.1).
   *
   *  Opt-in per column, not per table, because on a keyset-paged list most columns cannot be
   *  sorted honestly — see `lib/tableSort.ts`. A column is sortable when the caller can put the
   *  whole list in order, not when the header would look nice with an arrow. */
  sortable?: boolean;
  /** Put a filter control directly under this column's header (ADR-053, design-system §4.1).
   *
   *  Opt-in per column for the same reason `sortable` is, plus one of its own: a column with an
   *  unbounded value space must not offer a value list at all. `source_ip` is the named example —
   *  ADR-024 calls it out — and a facet query over it would enumerate the internet. */
  filter?: ColumnFilterSpec<T>;
}

interface Props<T> {
  rows: T[];
  columns: Column<T>[];
  rowKey: (row: T) => string;
  /** Called when scrolled near the end (load the next keyset page). */
  onReachEnd?: () => void;
  /** Row click (drill-down). */
  onRowClick?: (row: T) => void;
  /** Extra class(es) for one row — for a *state* the row is in, never for layout.
   *
   *  Added by ADR-053 Inc.5 so the Routing tables could keep their `is-muted` row while moving off
   *  hand-rolled `ytable` markup: a disabled channel or rule is dimmed, which is information, and a
   *  migration that silently dropped it would make every rule look live. Deliberately not a style
   *  hook — a class that changed a row's height would break the virtualizer's size estimate. */
  rowClass?: (row: T) => string | undefined;
  empty?: ReactNode;
  /** While the first page is in flight, show a loading placeholder instead of `empty` so an
   *  unloaded table never reads as "no rows". */
  loading?: boolean;
  /** Custom mobile card renderer (ADR-027). On mobile every DataTable renders as variable-height
   *  cards (a fixed multi-column grid can't fit ~390px); without this it falls back to a generic
   *  labeled key→value card built from `columns`. Provide this for a nicer per-screen card. Desktop
   *  always uses the grid. */
  renderCard?: (row: T) => ReactNode;
  /** Estimated card height (mobile card mode) before measurement; keeps the initial scrollbar sane. */
  cardEstimatePx?: number;
  /** The active sort, for the header affordance. Present only when some column is `sortable`.
   *
   *  ⚠️ **This component does not sort `rows`.** It draws the arrow and reports the click; the
   *  caller applies the order. That is deliberate: a keyset-paged table holds the pages that have
   *  been scrolled to, so sorting them here would reorder a prefix and present it as the order.
   *  `lib/tableSort.ts` has the two right answers and which list gets which. */
  sort?: SortState;
  /** Called with the next sort state when a sortable header is clicked. */
  onSortChange?: (next: SortState) => void;
  /** Current filter values, keyed by column key (ADR-053). Supplying this **and**
   *  `onFiltersChange` **and** at least one column with a `filter` spec is what draws the filter
   *  row — so every existing caller renders byte-for-byte what it did before.
   *
   *  ⚠️ Like `sort`, this component does not filter `rows`. It draws the controls and reports the
   *  change; the caller decides whether that means a client-side predicate
   *  (`lib/filterPredicate.ts`) or a query parameter. On a keyset-paged list only the caller knows
   *  which pages it holds, so filtering here would narrow a prefix and present it as the answer. */
  filters?: FilterState;
  onFiltersChange?: (next: FilterState) => void;
  /** Facet counts per column key, when the caller has them. See `lib/filterCounts.ts` for the
   *  counting rule — counts must exclude the column's own filter. */
  filterCounts?: Record<string, Record<string, number>>;
  /** Told which column's popover just opened, so a server-side caller can fetch that column's
   *  counts on demand instead of on every page load (ADR-023). */
  onFilterOpen?: (columnKey: string) => void;
  /** An editor that grows under a row when it is open, or `null` for a row that is not (ADR-053
   *  Inc.6 decision H). Metric sets and Device profiles both work this way.
   *
   *  ⚠️ **Supplying this switches the whole desktop body to *measured* heights.** Every row is then
   *  sized by a `ResizeObserver` rather than by the 44px estimate — the same path mobile cards have
   *  always used, not a new mechanism. That is the trade: an expandable table cannot also have a
   *  fixed-size virtualizer, so a table that does not need expansion must not pass this. */
  expanded?: (row: T) => ReactNode | null;
  /** The `rowKey` of the open row, or `null`. **Required for `expanded` to behave**, and it is not
   *  merely informational: a measured height is cached per index, so a row that collapses keeps
   *  reserving its expanded height until the cache is dropped. This value changing is what triggers
   *  that. One key rather than a set because both callers open exactly one row at a time. */
  expandedKey?: string | null;
  /** Let each row be as tall as its content instead of the standard 44px (ADR-078 増分 5).
   *
   *  For a table where one cell names a *variable number* of things — the alert-rule Scope column
   *  lists every profile a rule targets, one per line — a fixed row is a truncation rule in
   *  disguise: the names past the first are ellipsed away, and so is the "and N more" that was
   *  supposed to admit it. Opt in and the column can stack.
   *
   *  ⚠️ **This switches the desktop body to *measured* heights**, the same path cards and
   *  `expanded` already take: 44px stops being a promise the virtualizer relies on and becomes
   *  the pre-measurement estimate. Do not pass it to a table whose rows are uniform — the fixed
   *  path is cheaper, and it is what the data-table standard asks for (design-system §4.1). */
  autoRowHeight?: boolean;
}

const ROW_PX = 44; // comfortable-dense row height (matches the v2 .ytable standard)
const CARD_PX = 110; // default estimate for a mobile card before it is measured

export function DataTable<T>({
  rows,
  columns,
  rowKey,
  onReachEnd,
  onRowClick,
  rowClass,
  empty,
  loading,
  renderCard,
  cardEstimatePx = CARD_PX,
  sort,
  onSortChange,
  filters,
  onFiltersChange,
  filterCounts,
  onFilterOpen,
  expanded,
  expandedKey,
  autoRowHeight,
}: Props<T>) {
  const { t } = useTranslation();
  const scrollRef = useRef<HTMLDivElement>(null);
  // ⚠️ ONE const, used by THREE grids: `.dt-head`, `.dt-filters` and every `.dt-row`. If they ever
  // disagree the filter controls sit under the wrong columns, and nothing catches it — `.tsx` tests
  // never run (see .claude/rules/testing.md), so this shared binding IS the guard. Do not compute a
  // fourth grid template anywhere else, and do not let a track become `auto`: an `auto` track sizes
  // to its own content, so the three grids would resolve to different widths from the same string.
  const template = columns.map((c) => c.width ?? '1fr').join(' ');
  // ⚠️ …and the shared string is **not** enough on its own (ADR-054). Three grids resolve the same
  // template to different track widths once the pane is narrower than the columns need: a `1fr`
  // track then collapses to the item's min-content contribution, which here is only its padding —
  // 28px in `.dt-h`, 16px in `.dt-f` — so the header and the filter row landed their grid lines
  // 12px apart and the last columns were drawn past the edge of an `overflow: hidden` box, where
  // nothing could reach them. Handing all three the same `min-width` removes the room to differ and
  // turns the overflow into a scroll. Inert while the pane is wide enough, which is why the tables
  // that already fit are untouched.
  const minWidth = minTableWidth(columns);
  const widthStyle = minWidth > 0 ? { minWidth: `${minWidth}px` } : undefined;
  // Card mode whenever we're in mobile layout (respects the uiMode='desktop' override): the desktop
  // grid can't fit ~390px. A custom `renderCard` wins; otherwise a generic labeled card is built
  // from the columns. Desktop is byte-for-byte its previous grid self.
  const cardMode = useViewportMode() === 'mobile';

  // Heights are measured whenever they can vary — mobile cards always, an expandable table because
  // the open row is several times a closed one, and an `autoRowHeight` table because one of its
  // cells stacks. Elsewhere the fixed 44px estimate stands, so the existing tables keep the cheaper
  // path. `estimateSize` stays 44 either way: it is the pre-measurement guess that keeps the
  // initial scrollbar sane, not the final height.
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => (cardMode ? cardEstimatePx : ROW_PX),
    overscan: 12,
  });

  // Drop the cached measurements when the open row changes. Without this a row that has been
  // collapsed keeps the height it had while open — the list stays full of gaps, which reads as a
  // rendering bug rather than as a stale cache. `measure()` is a no-op for a table with no
  // expansion, and the dependency never changes there.
  useEffect(() => {
    if (expanded) virtualizer.measure();
    // `virtualizer` is stable for the life of the component; including it would re-run this on
    // every scroll frame.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [expandedKey]);

  // The filter row draws only when the caller asked for it AND some column opted in AND the shared
  // decision says the row is showing. That last half lives inside `ColumnFilterRow` (Inc.10) rather
  // than here, so this table and the five hand-rolled rows elsewhere read the *same* decision
  // instead of six look-alikes (four of which had no decision at all — see `MobileFilterSheet.tsx`).
  // It carries the desktop open/closed state too, so a second answer here would leave the row drawn
  // on a screen whose toggle says it is closed.
  const filterCols = useMemo(() => filterableColumns(columns), [columns]);
  // ⚠️ One slot per column, in the header's order — including the columns with no filter, which
  // become empty tracks. Skipping them would slide every later control out from under its header.
  const filterSlots = useMemo(
    () => columns.map((c) => (c.filter ? { key: c.key, align: c.align } : null)),
    [columns],
  );
  // The accessible name has to be a string, and `header` is a ReactNode. Every filterable column's
  // header is a `t()` string today; the key is a readable last resort rather than an empty label.
  const filterLabels = useMemo(
    () => Object.fromEntries(columns.map((c) => [c.key, typeof c.header === 'string' ? c.header : c.key])),
    [columns],
  );

  const items = virtualizer.getVirtualItems();
  // Fire the page-load callback once the last virtual row is within view of the end.
  if (onReachEnd && rows.length > 0) {
    const last = items[items.length - 1];
    if (last && last.index >= rows.length - 1) onReachEnd();
  }

  return (
    <div className="dt">
      {!cardMode && (
        <div className="dt-head" style={{ gridTemplateColumns: template, ...widthStyle }}>
          {columns.map((c) => {
            const cls = c.align === 'right' ? 'dt-h right' : 'dt-h';
            if (!c.sortable || !sort || !onSortChange) {
              return (
                <div key={c.key} className={cls}>
                  {c.header}
                </div>
              );
            }
            const active = sort.by === c.key;
            return (
              <button
                key={c.key}
                type="button"
                className={active ? `${cls} dt-h-sort active` : `${cls} dt-h-sort`}
                // The header is a real button, so the sort is keyboard-operable — an operator
                // driving the table from the keyboard is a stated requirement, and a click handler
                // on a `div` is not reachable that way.
                onClick={() => onSortChange(nextSort(sort, c.key))}
                aria-sort={active ? (sort.dir === 'asc' ? 'ascending' : 'descending') : 'none'}
              >
                {c.header}
                <span className="dt-h-arrow" aria-hidden="true">
                  {active ? (sort.dir === 'asc' ? '▲' : '▼') : '↕'}
                </span>
              </button>
            );
          })}
        </div>
      )}
      {!!filters && !!onFiltersChange && (
        <ColumnFilterRow
          columns={filterCols}
          slots={filterSlots}
          filters={filters}
          onChange={onFiltersChange}
          counts={filterCounts}
          onFilterOpen={onFilterOpen}
          labels={filterLabels}
          className="dt-filters"
          style={{ gridTemplateColumns: template, ...widthStyle }}
        />
      )}
      {/* The scroller carries the same floor as the two grids above it, or the rows would stop
          short of the header the moment the table is scrolled sideways. Not in card mode: cards
          stack to the viewport, and a 1,300px floor on a phone would invent a horizontal scroll
          for a layout that has no columns to hold up. */}
      <div className="dt-scroll scroll-y" ref={scrollRef} style={cardMode ? undefined : widthStyle}>
        {rows.length === 0 ? (
          <div className="dt-empty">{loading ? t('loading') : (empty ?? t('noRows'))}</div>
        ) : (
          <div className="dt-body" style={{ height: virtualizer.getTotalSize() }}>
            {items.map((vi) => {
              const row = rows[vi.index];
              if (cardMode) {
                // Variable-height card: measured by the virtualizer (data-index + measureElement).
                return (
                  <div
                    key={rowKey(row)}
                    data-index={vi.index}
                    ref={virtualizer.measureElement}
                    className={onRowClick ? 'dt-card clickable' : 'dt-card'}
                    style={{ transform: `translateY(${vi.start}px)` }}
                    onClick={onRowClick ? () => onRowClick(row) : undefined}
                  >
                    {renderCard ? (
                      renderCard(row)
                    ) : (
                      // Generic fallback card: each column as a labeled key→value pair.
                      <dl className="dt-card-auto">
                        {columns.map((c) => (
                          <div key={c.key} className="dt-card-pair">
                            <dt className="dt-card-k">{c.header}</dt>
                            <dd className="dt-card-v">{c.render(row)}</dd>
                          </div>
                        ))}
                      </dl>
                    )}
                  </div>
                );
              }
              const cells = columns.map((c) => (
                <div key={c.key} className={c.align === 'right' ? 'dt-cell right' : 'dt-cell'}>
                  {c.render(row)}
                </div>
              ));
              const rowCls = [
                'dt-row',
                // 🚨 The 44px in `.dt-row` is the virtualizer's position arithmetic, not a
                // decoration — a taller cell without this class overflows into the next row
                // rather than pushing it down.
                autoRowHeight ? 'dt-row-auto' : '',
                onRowClick ? 'clickable' : '',
                rowClass?.(row) ?? '',
              ]
                .filter(Boolean)
                .join(' ');
              if (!expanded) {
                // `data-index` + the measuring ref only when the caller asked for auto heights:
                // react-virtual reads the attribute to know which index it just measured, and a
                // fixed-size table must not report heights at all or every row would be measured
                // on every scroll frame for an answer that is already known.
                return (
                  <div
                    key={rowKey(row)}
                    data-index={autoRowHeight ? vi.index : undefined}
                    ref={autoRowHeight ? virtualizer.measureElement : undefined}
                    className={rowCls}
                    style={{
                      gridTemplateColumns: template,
                      transform: `translateY(${vi.start}px)`,
                    }}
                    onClick={onRowClick ? () => onRowClick(row) : undefined}
                  >
                    {cells}
                  </div>
                );
              }
              // Expandable: the row and its editor share ONE positioned wrapper, and the wrapper is
              // what the virtualizer measures. Two separate virtual items would let a scroll land
              // between a row and its own editor.
              const body = expanded(row);
              return (
                <div
                  key={rowKey(row)}
                  data-index={vi.index}
                  ref={virtualizer.measureElement}
                  className="dt-group"
                  style={{ transform: `translateY(${vi.start}px)` }}
                >
                  <div
                    className={rowCls}
                    style={{ gridTemplateColumns: template }}
                    onClick={onRowClick ? () => onRowClick(row) : undefined}
                  >
                    {cells}
                  </div>
                  {body !== null && body !== undefined && (
                    <div className="dt-expansion">{body}</div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
