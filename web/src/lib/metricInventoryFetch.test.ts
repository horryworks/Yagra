// SPDX-License-Identifier: AGPL-3.0-only
// `fetchNodeMetrics` against a mocked client. Apart from `metricInventoryCache.test.ts` on purpose:
// that file tests the name memory with no module mocked, and a `vi.mock` is file-wide.

import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { NodeMetricEntry } from '../types/api';

const { listNodeMetrics } = vi.hoisted(() => ({ listNodeMetrics: vi.fn() }));
vi.mock('../services/api', () => ({ api: { listNodeMetrics } }));

import { fetchNodeMetrics } from './metricInventoryCache';

const entry = (metric: string): NodeMetricEntry => ({
  metric,
  metric_kind: 'gauge',
  dimension: 'none',
  status: 'ok',
  series_count: 1,
});

beforeEach(() => {
  vi.clearAllMocks();
});

describe('fetchNodeMetrics', () => {
  it('rejects when the read fails — a failed read is not an empty inventory', async () => {
    // 🚨 It used to resolve `[]`, which the Collection tab reads as "Collection failing" and the
    // Overview reads as "this device has no health to show".
    listNodeMetrics.mockRejectedValue(new Error('503'));
    await expect(fetchNodeMetrics('n-fail')).rejects.toThrow('503');
  });

  it('does not remember a failure: the next call asks again and can succeed', async () => {
    listNodeMetrics.mockRejectedValueOnce(new Error('503'));
    await expect(fetchNodeMetrics('n-retry', 60_000)).rejects.toThrow();
    listNodeMetrics.mockResolvedValue([entry('cpu')]);
    await expect(fetchNodeMetrics('n-retry', 60_000)).resolves.toEqual([entry('cpu')]);
    expect(listNodeMetrics).toHaveBeenCalledTimes(2);
  });

  it('gives concurrent callers the same rejection from one request', async () => {
    listNodeMetrics.mockRejectedValue(new Error('503'));
    const [a, b] = await Promise.allSettled([fetchNodeMetrics('n-both'), fetchNodeMetrics('n-both')]);
    expect(a.status).toBe('rejected');
    expect(b.status).toBe('rejected');
    expect(listNodeMetrics).toHaveBeenCalledTimes(1);
  });

  it('still resolves an empty inventory as empty — that one is an answer', async () => {
    listNodeMetrics.mockResolvedValue([]);
    await expect(fetchNodeMetrics('n-empty')).resolves.toEqual([]);
  });
});
