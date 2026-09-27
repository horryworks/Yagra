// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { previewBatches } from './previewBatch';
import { SWEEP_LIMIT } from './cidr';

describe('previewBatches', () => {
  it('asks about a page of addresses in one request', () => {
    const page = Array.from({ length: 100 }, (_, i) => `192.0.2.${i + 1}`);
    const got = previewBatches(page, new Set());
    expect(got).toHaveLength(1);
    expect(got[0]).toEqual(page);
  });

  it('asks about each address once, and never again once asked', () => {
    expect(
      previewBatches(['192.0.2.1', '192.0.2.1', '', '192.0.2.2'], new Set(['192.0.2.2'])),
    ).toEqual([['192.0.2.1']]);
    expect(previewBatches(['192.0.2.2'], new Set(['192.0.2.2']))).toEqual([]);
  });

  it('cuts at the endpoint cap', () => {
    expect(previewBatches(['a', 'b', 'c', 'd', 'e'], new Set(), 2)).toEqual([
      ['a', 'b'],
      ['c', 'd'],
      ['e'],
    ]);
    expect(SWEEP_LIMIT).toBe(4096);
  });
});
