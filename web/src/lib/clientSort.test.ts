// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { clientSortValues, specSortValue, type ClientSortColumn } from './clientSort';
import { sortRows } from './tableSort';

interface Row {
  name: string;
  sev: string;
  tags: string[];
  at: number;
  port: string;
}

const rows: Row[] = [
  { name: 'sw-10', sev: 'info', tags: [], at: 30, port: '9' },
  { name: 'sw-2', sev: 'critical', tags: ['b'], at: 10, port: '10' },
  { name: 'ap-1', sev: 'bogus', tags: ['a'], at: 20, port: '' },
];

const columns: ClientSortColumn<Row>[] = [
  {
    key: 'name',
    filter: { kind: 'text', modes: ['contains'], readText: (r) => [r.name] },
  },
  {
    key: 'sev',
    filter: {
      kind: 'enum',
      allLabel: 'All',
      options: [
        { value: 'critical', label: 'Critical' },
        { value: 'warning', label: 'Warning' },
        { value: 'info', label: 'Info' },
      ],
      readValue: (r) => r.sev,
    },
  },
  { key: 'tags', filter: { kind: 'enum', allLabel: 'All', options: [], readValue: (r) => r.tags } },
  { key: 'at', sortValue: (r) => r.at },
  {
    key: 'port',
    filter: { kind: 'values', parse: (t) => t, readValues: (r) => r.port },
  },
  // Server-side: no reader, so nothing to sort on in the browser.
  { key: 'remote', filter: { kind: 'text', modes: ['contains'] } },
  { key: 'actions' },
];

const values = clientSortValues(columns);
const names = (out: Row[]) => out.map((r) => r.name);

describe('client sort values', () => {
  it('offers exactly the columns that can say what a row is worth', () => {
    expect(Object.keys(values)).toEqual(['name', 'sev', 'tags', 'at', 'port']);
  });

  it('orders an enum by its options, with a value they do not list after the rest', () => {
    expect(names(sortRows(rows, { by: 'sev', dir: 'asc' }, values))).toEqual(['sw-2', 'sw-10', 'ap-1']);
  });

  it('orders text by what the filter reads, numbers inside it numerically', () => {
    expect(names(sortRows(rows, { by: 'name', dir: 'asc' }, values))).toEqual(['ap-1', 'sw-2', 'sw-10']);
  });

  it('orders values as numbers, and a row with none last in both directions', () => {
    expect(names(sortRows(rows, { by: 'port', dir: 'asc' }, values))).toEqual(['sw-10', 'sw-2', 'ap-1']);
    expect(names(sortRows(rows, { by: 'port', dir: 'desc' }, values))).toEqual(['sw-2', 'sw-10', 'ap-1']);
  });

  it('lets a column state its own value', () => {
    expect(names(sortRows(rows, { by: 'at', dir: 'desc' }, values))).toEqual(['sw-10', 'ap-1', 'sw-2']);
  });

  it('reads the first of a list-valued enum and puts an empty list last', () => {
    expect(names(sortRows(rows, { by: 'tags', dir: 'asc' }, values)).at(-1)).toBe('sw-10');
  });

  it('derives nothing for a spec with no reader', () => {
    expect(specSortValue({ kind: 'number' })).toBeUndefined();
  });
});
