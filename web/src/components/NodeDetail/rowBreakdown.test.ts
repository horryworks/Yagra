// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { followedRow, memRows, visibleRows } from './rowBreakdown';

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
