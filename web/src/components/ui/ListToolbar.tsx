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
import { ClearFilters } from './ClearFilters';
import { FilterButton, MobileFilterSheet } from './MobileFilterSheet';
import { ResultCount, TableSpacer, TableToolbar } from './TableToolbar';
import { resultCount, type ToolbarFilters } from '../../lib/listToolbar';

interface ControlsProps<T> {
  list: ToolbarFilters<T>;
  /** What the sheet calls each filterable column — `columnLabels(columns)` for most screens. */
  labels: Record<string, string>;
  /** Runs as the phone's sheet opens: a server-counted list fetches every column's counts here,
   *  since the sheet shows them all at once (Events). */
  onSheetOpen?: () => void;
}

/** The filter button, "clear all filters", and the phone's filter sheet. */
export function FilterControls<T>({ list, labels, onSheetOpen }: ControlsProps<T>) {
  const [sheet, setSheet] = useState(false);
  return (
    <>
      <FilterButton
        columns={list.filterCols}
        filters={list.filters}
        onOpen={() => {
          onSheetOpen?.();
          setSheet(true);
        }}
      />
      <ClearFilters
        columns={list.filterCols}
        filters={list.filters}
        onClear={list.clear}
        extraActive={list.extraActive}
      />
      {sheet && (
        <MobileFilterSheet
          columns={list.filterCols}
          filters={list.filters}
          onChange={list.setFilters}
          counts={list.counts}
          labels={labels}
          onClose={() => setSheet(false)}
        />
      )}
    </>
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
  /** Before the filter controls: a scope picker, a section title. */
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
  count,
  leading,
  tools,
  note,
  children,
}: Props<T>) {
  const n = count && resultCount({ ...count, anyFiltered: list.anyFiltered });
  return (
    <TableToolbar>
      {leading}
      <FilterControls list={list} labels={labels} onSheetOpen={onSheetOpen} />
      {tools}
      <TableSpacer />
      {note}
      {count && n && <ResultCount shown={n.shown} total={n.total} noun={count.noun(n.nounCount)} />}
      {children}
    </TableToolbar>
  );
}
