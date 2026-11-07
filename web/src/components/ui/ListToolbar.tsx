// SPDX-License-Identifier: AGPL-3.0-only
// A list's action row, drawn once (ADR-184 increment 28). The decisions are `lib/listToolbar.ts`.
//
// Two components, because two shapes of container exist:
//  - `ListToolbar` is the `.table-toolbar` row every list screen draws, in the order the design
//    system fixes: leading control (a picker, a section title) → filter controls → tools → spacer →
//    note → result count → actions.
//  - `FilterControls` is only the filter button, "clear all filters" and the sheet — for the three
//    node-detail tabs that sit in a toolbar of their own (`.nd-if-toolbar` and friends).
//
// **The sheet is mounted here, beside the button that opens it** (T5). It used to be mounted after
// the table, by each screen, with the open flag in the screen's state — thirty-five copies of a
// `useState`, and a sheet whose labels each screen assembled on its own. `Modal` does not portal,
// so where it mounts matters: every toolbar container it now mounts inside was checked to hold no
// `transform`, `filter` or `overflow` that would clip or re-anchor a fixed overlay, and
// `tests/ui/filterSheet.spec.ts` presses it on a phone.
//
// ⚠️ **`DataTable` stays on the screen.** `tableIds.test.ts` reads each table's id as a literal at
// its tag; a wrapper passing the id through as a variable would read as an unnamed table.
import { useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { ClearFilters } from './ClearFilters';
import { FilterButton, MobileFilterSheet } from './MobileFilterSheet';
import { ResultCount, TableSpacer, TableToolbar } from './TableToolbar';
import { filterChips, resultCount, type ToolbarFilters } from '../../lib/listToolbar';
import { clearFilter, type FilterableColumn } from '../../lib/columnFilter';
import { filterSummaryText } from '../../lib/filterWords';
import './FilterChips.css';
import './Section.css';

interface ControlsProps<T> {
  list: ToolbarFilters<T>;
  /** What the sheet calls each filterable column — `columnLabels(columns)` for most screens. */
  labels: Record<string, string>;
  /** Runs as the phone's sheet opens: a server-counted list fetches every column's counts here,
   *  since the sheet shows them all at once (Events). */
  onSheetOpen?: () => void;
  /** The list is drawn without a header row, so its filters open from one funnel here — all of
   *  them (`true`), or only the ones that have no column to sit on (Device profiles' category). */
  menu?: boolean | readonly FilterableColumn<T>[];
}

/** The filter button, the chips naming each set filter, "clear all filters", and the phone's
 *  filter sheet (ADR-228). A list with a header row filters from each column's funnel, so on a
 *  desktop it gets no button; a list drawn without one passes `menu` and gets a funnel here. */
export function FilterControls<T>({ list, labels, onSheetOpen, menu }: ControlsProps<T>) {
  const [sheet, setSheet] = useState(false);
  const sections = {
    columns: list.filterCols,
    labels,
    filters: list.filters,
    onChange: list.setFilters,
    counts: list.counts,
  };
  return (
    <>
      <FilterButton
        columns={list.filterCols}
        filters={list.filters}
        onOpen={() => {
          onSheetOpen?.();
          setSheet(true);
        }}
        menu={menu ? { ...sections, columns: menu === true ? list.filterCols : menu } : undefined}
      />
      <FilterChips list={list} labels={labels} />
      <ClearFilters
        columns={list.filterCols}
        filters={list.filters}
        onClear={list.clear}
        extraActive={list.extraActive}
      />
      {sheet && <MobileFilterSheet {...sections} onClose={() => setSheet(false)} />}
    </>
  );
}

/** One chip per column that is narrowing the list: its name, what it is set to, and a × that
 *  clears that column alone. This is where the closed filter-row trigger's job went - saying which
 *  filters are on - now that the controls open from the headers. */
function FilterChips<T>({
  list,
  labels,
}: {
  list: ToolbarFilters<T>;
  labels: Record<string, string>;
}) {
  const { t } = useTranslation('common');
  const chips = filterChips(list.filterCols, list.filters);
  if (chips.length === 0) return null;
  return (
    <span className="filter-chips">
      {chips.map((c) => {
        const name = labels[c.key] ?? c.key;
        const mark = c.not ? '≠ ' : '';
        const value = mark + filterSummaryText(c.summary, t);
        return (
          <span className="filter-chip" key={c.key} title={`${name}: ${value}`}>
            <span className="filter-chip-name">{name}</span>
            <span className="filter-chip-value">{value}</span>
            <button
              type="button"
              className="filter-chip-x"
              aria-label={t('filter.clearAria', { column: name })}
              onClick={() => list.setFilters(clearFilter(list.filterCols, list.filters, c.key))}
            >
              ✕
            </button>
          </span>
        );
      })}
    </span>
  );
}

interface Props<T> extends ControlsProps<T> {
  /** The result count. `total` is the unfiltered size; it is shown only while filtered (T2), and
   *  `noun` is given the number it stands beside (T1). Omit on a list with nothing to count. */
  count?: {
    shown: number;
    total?: number;
    noun: (n: number) => string;
    showTotal?: 'whenFiltered' | 'always';
  };
  /** The heading of a list that is one section of a page (Routing's channels, its rules), drawn at
   *  the start of this row so the count and the buttons share its line. The same look as a
   *  page-level `SectionHead` (`level={2}`). */
  title?: ReactNode;
  /** Before the filter controls: a scope picker. */
  leading?: ReactNode;
  /** After the filter controls, before the spacer: a sort control, a view switch. */
  tools?: ReactNode;
  /** After the spacer, before the count: a short remark about the list ("showing the first 500"). */
  note?: ReactNode;
  /** The actions, last: `+ Add`, an export. */
  children?: ReactNode;
}

export function ListToolbar<T>({
  list,
  labels,
  onSheetOpen,
  menu,
  count,
  title,
  leading,
  tools,
  note,
  children,
}: Props<T>) {
  const n = count && resultCount({ ...count, anyFiltered: list.anyFiltered });
  return (
    <TableToolbar>
      {title !== undefined && <h2 className="ui-section-title is-page">{title}</h2>}
      {leading}
      <FilterControls list={list} labels={labels} onSheetOpen={onSheetOpen} menu={menu} />
      {tools}
      <TableSpacer />
      {note}
      {count && n && <ResultCount shown={n.shown} total={n.total} noun={count.noun(n.nounCount)} />}
      {children}
    </TableToolbar>
  );
}
