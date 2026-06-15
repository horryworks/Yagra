// High-density, virtualized table (§4: lists assume tens of thousands of rows ⇒ virtual
// scroll + server-side paging; never render the whole set). Generic over the row type. The
// caller fetches pages (keyset) and appends to `rows`; this component windows the DOM with
// @tanstack/react-virtual and calls `onReachEnd` when the user nears the bottom so the
// parent can load the next page.

import { useRef } from 'react';
import type { ReactNode } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
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
}

interface Props<T> {
  rows: T[];
  columns: Column<T>[];
  rowKey: (row: T) => string;
  /** Called when scrolled near the end (load the next keyset page). */
  onReachEnd?: () => void;
  /** Row click (drill-down). */
  onRowClick?: (row: T) => void;
  empty?: ReactNode;
  /** While the first page is in flight, show a loading placeholder instead of `empty` so an
   *  unloaded table never reads as "no rows". */
  loading?: boolean;
}

const ROW_PX = 44; // comfortable-dense row height (matches the v2 .ytable standard)

export function DataTable<T>({
  rows,
  columns,
  rowKey,
  onReachEnd,
  onRowClick,
  empty,
  loading,
}: Props<T>) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const template = columns.map((c) => c.width ?? '1fr').join(' ');

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_PX,
    overscan: 12,
  });

  const items = virtualizer.getVirtualItems();
  // Fire the page-load callback once the last virtual row is within view of the end.
  if (onReachEnd && rows.length > 0) {
    const last = items[items.length - 1];
    if (last && last.index >= rows.length - 1) onReachEnd();
  }

  return (
    <div className="dt">
      <div className="dt-head" style={{ gridTemplateColumns: template }}>
        {columns.map((c) => (
          <div key={c.key} className={c.align === 'right' ? 'dt-h right' : 'dt-h'}>
            {c.header}
          </div>
        ))}
      </div>
      <div className="dt-scroll scroll-y" ref={scrollRef}>
        {rows.length === 0 ? (
          <div className="dt-empty">{loading ? 'Loading…' : (empty ?? 'No rows.')}</div>
        ) : (
          <div className="dt-body" style={{ height: virtualizer.getTotalSize() }}>
            {items.map((vi) => {
              const row = rows[vi.index];
              return (
                <div
                  key={rowKey(row)}
                  className={onRowClick ? 'dt-row clickable' : 'dt-row'}
                  style={{
                    gridTemplateColumns: template,
                    transform: `translateY(${vi.start}px)`,
                  }}
                  onClick={onRowClick ? () => onRowClick(row) : undefined}
                >
                  {columns.map((c) => (
                    <div key={c.key} className={c.align === 'right' ? 'dt-cell right' : 'dt-cell'}>
                      {c.render(row)}
                    </div>
                  ))}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
