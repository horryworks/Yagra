// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  followedRow,
  limitRows,
  memHeadline,
  memRangeQuery,
  memRows,
  ROW_LIST_MAX,
  visibleRows,
  type MemRow,
} from './rowBreakdown';

describe('limitRows', () => {
  const rows = (n: number) => Array.from({ length: n }, (_, i) => ({ row: i, value: n - i }));

  // ADR-143 Inc.2: a 64-core host listed 64 lines under one card.
  it('lists the first five and counts the rest', () => {
    const { shown, more } = limitRows(rows(64));
    expect(ROW_LIST_MAX).toBe(5);
    expect(shown.map((r) => r.row)).toEqual([0, 1, 2, 3, 4]);
    expect(more).toBe(59);
  });

  // The accepting edge: exactly the cap lists everything and leaves no "and N more" line behind.
  it('leaves nothing out when there are no more rows than the cap', () => {
    expect(limitRows(rows(5))).toEqual({ shown: rows(5), more: 0 });
    expect(limitRows(rows(2))).toEqual({ shown: rows(2), more: 0 });
  });

  it('counts one left out when there is one more than the cap', () => {
    const { shown, more } = limitRows(rows(6));
    expect(shown).toHaveLength(5);
    expect(more).toBe(1);
  });

  // The headline is `rows[0]`, so the cut must never be able to drop it.
  it('keeps the first row, which is what the card headlines', () => {
    expect(limitRows(rows(64)).shown[0]).toEqual(rows(64)[0]);
  });
});

describe('memRows', () => {
  // The C2960S's own numbers (PoC fleet, 2026-09-15): the pools the old card could not tell apart.
  const used = [
    { row: 1, name: 'Processor', value: 37_548_112 },
    { row: 2, name: 'I/O', value: 12_312_508 },
    { row: 20, name: 'Driver text', value: 40 },
  ];
  const free = [
    { row: 1, value: 29_059_400 },
    { row: 2, value: 2_367_556 },
    { row: 20, value: 1_048_536 },
  ];

  it('joins each pool on its own row and puts the fullest first', () => {
    const rows = memRows('cisco', ['cisco_mem_used', 'cisco_mem_free'], 1, used, free);
    expect(rows.map((r) => r.name)).toEqual(['I/O', 'Processor', 'Driver text']);
    expect(rows[0].pct).toBeCloseTo(83.87, 1);
    expect(rows[1].pct).toBeCloseTo(56.37, 1);
    expect(rows[1].totalBytes).toBe(37_548_112 + 29_059_400);
  });

  // 🚨 The defect this replaces: the largest "used" over the largest "used + free" is 56%, and the
  // pool at 83.9% is not in that number at all.
  it('never divides one row by another', () => {
    const rows = memRows('cisco', ['cisco_mem_used', 'cisco_mem_free'], 1, used, free);
    const maxUsed = Math.max(...used.map((r) => r.value));
    const maxFree = Math.max(...free.map((r) => r.value));
    const folded = (100 * maxUsed) / (maxUsed + maxFree);
    expect(rows.every((r) => Math.abs(r.pct - folded) > 0.001 || r.row === 1)).toBe(true);
    expect(rows[0].pct).toBeGreaterThan(folded + 20);
  });

  it('leaves out a pool with no total and a row with no other half', () => {
    const rows = memRows(
      'cisco-cemp',
      ['cisco_cemp_mem_used', 'cisco_cemp_mem_free'],
      1,
      [
        { row: 227_729_484, name: 'DP System memory', value: 2_233_538_976 },
        { row: 177_396_627, name: 'MEMPOOL_GLOBAL_SHARED', value: 0 },
        { row: 5, value: 10 },
      ],
      [
        { row: 227_729_484, value: 13_455_054_432 },
        { row: 177_396_627, value: 0 },
      ],
    );
    expect(rows.map((r) => r.name)).toEqual(['DP System memory']);
  });

  it('reads a Huawei pair as total and free', () => {
    const rows = memRows(
      'huawei',
      ['huawei_mem_total', 'huawei_mem_free'],
      1,
      [{ row: 3, name: null, value: 1000 }],
      [{ row: 3, value: 250 }],
    );
    expect(rows).toEqual([{ row: 3, name: null, usedBytes: 750, totalBytes: 1000, pct: 75 }]);
  });
});

const pool = (row: number, pct: number): MemRow => ({
  row,
  name: null,
  usedBytes: pct,
  totalBytes: 100,
  pct,
});

describe('memHeadline', () => {
  // 🚨 The C2960S case: the node-wide maxima read 56% while the fullest pool read 83.9%. While there
  // are pools the node-wide reading is not consulted at all.
  it('headlines the fullest pool, never the node-wide reading, while there are pools', () => {
    let consulted = false;
    const headline = memHeadline([pool(2, 83.9), pool(1, 56.4)], () => {
      consulted = true;
      return { usedBytes: 1, totalBytes: 2, pct: 56 };
    });
    expect(headline.pct).toBe(83.9);
    expect(consulted).toBe(false);
  });

  it('falls back to the node-wide reading when there are no pools', () => {
    const nodeWide = { usedBytes: 10, totalBytes: 20, pct: 50 };
    expect(memHeadline([], () => nodeWide)).toBe(nodeWide);
  });
});

describe('memRangeQuery', () => {
  it('charts the fullest pool by its row when there are several', () => {
    expect(memRangeQuery([pool(2, 83.9), pool(1, 56.4)])).toEqual({ row: 2 });
  });

  // One pool may be a scalar source read back as row 0, which has no series of its own to chart.
  it('charts the node-wide maximum with one pool or none', () => {
    expect(memRangeQuery([pool(0, 40)])).toEqual({ agg: 'max' });
    expect(memRangeQuery([])).toEqual({ agg: 'max' });
  });
});

describe('visibleRows', () => {
  it('drops zero and non-numbers and puts the highest first', () => {
    expect(
      visibleRows([
        { row: 9, value: 0 },
        { row: 7, name: 'MPU Board 0', value: 33 },
        { row: 8, name: 'MPU Board 1', value: 41 },
        { row: 10, value: Number.NaN },
      ]).map((r) => r.row),
    ).toEqual([8, 7]);
  });
});

describe('followedRow', () => {
  it('follows the first row only when there is more than one', () => {
    expect(followedRow([{ row: 2 }, { row: 1 }])).toEqual({ row: 2 });
    // A scalar reads back as row 0, which has no series of its own to chart.
    expect(followedRow([{ row: 0 }])).toBeNull();
    expect(followedRow([])).toBeNull();
  });
});
