// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for facet counting (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import { TEXT_MODES, type FilterableColumn } from './columnFilter';
import { facetCounts, rowsExcluding } from './filterCounts';

interface Row {
  kind: string;
  action: string;
  message: string;
}

const ROWS: Row[] = [
  { kind: 'syslog', action: 'fired', message: 'link down' },
  { kind: 'syslog', action: 'none', message: 'link up' },
  { kind: 'syslog', action: 'none', message: 'bgp up' },
  { kind: 'trap', action: 'fired', message: 'linkDown' },
  { kind: 'trap', action: 'none', message: 'coldStart' },
  { kind: 'webhook', action: 'none', message: 'deploy' },
];

const COLUMNS: FilterableColumn<Row>[] = [
  {
    key: 'kind',
    filter: {
      kind: 'enum',
      options: ['syslog', 'trap', 'webhook'].map((v) => ({ value: v, label: v })),
      readValue: (r) => r.kind,
      allLabel: 'All kinds',
      counts: 'client',
    },
  },
  {
    key: 'action',
    filter: {
      kind: 'enum',
      options: ['fired', 'none'].map((v) => ({ value: v, label: v })),
      readValue: (r) => r.action,
      allLabel: 'All results',
      counts: 'client',
    },
  },
  {
    key: 'message',
    filter: { kind: 'text', modes: TEXT_MODES, not: true, readText: (r) => [r.message] },
  },
];

const NOW = 0;

describe('a facet excludes its OWN filter', () => {
  it('keeps every option countable after one of them is selected', () => {
    // The bug this prevents: count the *displayed* rows and selecting `syslog` shows `trap: 0`, so
    // the operator is told the thing they might switch to is empty when it has rows. Excel has
    // always excluded the column's own filter, and that is the reading that stays usable.
    expect(facetCounts(ROWS, COLUMNS, {}, 'kind', NOW)).toEqual({
      syslog: 3,
      trap: 2,
      webhook: 1,
    });
    expect(facetCounts(ROWS, COLUMNS, { kind: 'syslog' }, 'kind', NOW)).toEqual({
      syslog: 3,
      trap: 2,
      webhook: 1,
    });
  });

  it('still honours every OTHER column filter', () => {
    // Excluding its own filter is not the same as ignoring all of them — the counts must describe
    // what selecting the option would actually produce.
    expect(facetCounts(ROWS, COLUMNS, { action: 'fired' }, 'kind', NOW)).toEqual({
      syslog: 1,
      trap: 1,
      webhook: 0,
    });
    expect(facetCounts(ROWS, COLUMNS, { message: 'link' }, 'kind', NOW)).toEqual({
      syslog: 2,
      trap: 1,
      webhook: 0,
    });
  });

  it('keeps a zero option in the map rather than dropping it', () => {
    // A checkbox that vanishes at zero cannot be un-selected, and a list whose length changes as
    // you click it is unusable.
    const counts = facetCounts(ROWS, COLUMNS, { message: 'deploy' }, 'kind', NOW);
    expect(Object.keys(counts).sort()).toEqual(['syslog', 'trap', 'webhook']);
    expect(counts.syslog).toBe(0);
    expect(counts.webhook).toBe(1);
  });

  it('sums to the row count when nothing else narrows and values are single', () => {
    const counts = facetCounts(ROWS, COLUMNS, {}, 'action', NOW);
    expect(Object.values(counts).reduce((a, b) => a + b, 0)).toBe(ROWS.length);
  });
});

describe('rowsExcluding', () => {
  it('drops exactly one column from the predicate', () => {
    const state = { kind: 'syslog', action: 'fired' };
    expect(rowsExcluding(ROWS, COLUMNS, state, 'kind', NOW)).toHaveLength(2); // both `fired` rows
    expect(rowsExcluding(ROWS, COLUMNS, state, 'action', NOW)).toHaveLength(3); // all syslog rows
  });
});

describe('a column with no enum spec', () => {
  it('counts nothing rather than guessing', () => {
    expect(facetCounts(ROWS, COLUMNS, {}, 'message', NOW)).toEqual({});
    expect(facetCounts(ROWS, COLUMNS, {}, 'nonexistent', NOW)).toEqual({});
  });
});
