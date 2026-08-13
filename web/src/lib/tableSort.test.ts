// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the DataTable sort helpers (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import { nextSort, sortRows, type SortState, type SortValues } from './tableSort';

interface Row {
  name: string;
  n: number;
  when: string | null;
}

const rows: Row[] = [
  { name: 'sw-10', n: 10, when: '2026-01-03T00:00:00Z' },
  { name: 'sw-2', n: 2, when: null },
  { name: 'Ålesund', n: 5, when: '2026-01-01T00:00:00Z' },
  { name: 'apex', n: 5, when: '2026-01-02T00:00:00Z' },
];

const values: SortValues<Row> = {
  name: (r) => r.name,
  n: (r) => r.n,
  when: (r) => r.when,
};

const names = (out: Row[]) => out.map((r) => r.name);
const asc = (by: string): SortState => ({ by, dir: 'asc' });
const desc = (by: string): SortState => ({ by, dir: 'desc' });

describe('nextSort', () => {
  it('flips direction on the active column', () => {
    expect(nextSort(asc('name'), 'name')).toEqual(desc('name'));
    expect(nextSort(desc('name'), 'name')).toEqual(asc('name'));
  });

  it('starts a new column ascending rather than inheriting the old direction', () => {
    // Carrying a `desc` over from an unrelated column reads as the click not having worked.
    expect(nextSort(desc('name'), 'n')).toEqual(asc('n'));
  });

  it('has no unsorted third state', () => {
    // A table always has some order, and "back to whatever the server sent" is a state an operator
    // cannot name — so clicking twice returns to ascending, never to nothing.
    let s = asc('name');
    s = nextSort(s, 'name');
    s = nextSort(s, 'name');
    expect(s).toEqual(asc('name'));
  });
});

describe('sortRows', () => {
  it('does not mutate its input', () => {
    // The array usually comes straight from a store or a fetch; sorting in place would reorder
    // something another component is rendering from.
    const before = [...rows];
    sortRows(rows, asc('name'), values);
    expect(rows).toEqual(before);
  });

  it('compares numbers as numbers', () => {
    expect(sortRows(rows, asc('n'), values).map((r) => r.n)).toEqual([2, 5, 5, 10]);
  });

  it('compares strings numerically, so sw-2 precedes sw-10', () => {
    // Code-point order puts "sw-10" first, which is the classic wrong-looking table.
    const sorted = names(sortRows(rows, asc('name'), values));
    expect(sorted.indexOf('sw-2')).toBeLessThan(sorted.indexOf('sw-10'));
  });

  it('collates accents beside their base letter', () => {
    // Code-point order banishes "Ålesund" past "z". `localeCompare` is what stops the operator
    // hunting for a node at the bottom of the list.
    const sorted = names(sortRows(rows, asc('name'), values));
    expect(sorted.indexOf('Ålesund')).toBeLessThan(sorted.indexOf('sw-2'));
  });

  it('keeps missing values last in BOTH directions', () => {
    // Flipping the direction must not fill the top of the screen with blanks: the operator wants
    // the other end of the data, not the rows that have none.
    expect(names(sortRows(rows, asc('when'), values)).at(-1)).toBe('sw-2');
    expect(names(sortRows(rows, desc('when'), values)).at(-1)).toBe('sw-2');
  });

  it('treats an empty string as missing, not as the smallest value', () => {
    const withBlank: Row[] = [
      { name: 'b', n: 1, when: '' },
      { name: 'a', n: 2, when: '2026-01-01T00:00:00Z' },
    ];
    expect(names(sortRows(withBlank, asc('when'), values))).toEqual(['a', 'b']);
  });

  it('is stable, so equal rows keep the order they arrived in', () => {
    // Two rows share n=5. Re-sorting must not shuffle them — a table that reshuffles its ties on
    // every render looks like it is refreshing when nothing changed.
    const once = names(sortRows(rows, asc('n'), values));
    const twice = names(sortRows(sortRows(rows, asc('n'), values), asc('n'), values));
    expect(twice).toEqual(once);
    expect(once.slice(1, 3)).toEqual(['Ålesund', 'apex']);
  });

  it('keeps ties in arrival order even when descending', () => {
    // The tie-break is deliberately NOT reversed with the sort: an operator flipping the direction
    // expects the ranking to invert, not the rows that were never ranked against each other.
    const out = names(sortRows(rows, desc('n'), values));
    expect(out.slice(1, 3)).toEqual(['Ålesund', 'apex']);
  });

  it('returns a copy unchanged when the column has no comparator', () => {
    // A column marked `sortable` with no entry in the value map is a wiring mistake. Returning the
    // rows untouched keeps the table usable while it is wrong, rather than throwing mid-render.
    expect(names(sortRows(rows, asc('nope'), values))).toEqual(names(rows));
  });
});
