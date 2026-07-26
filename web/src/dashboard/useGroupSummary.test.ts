// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { FleetGroupSummary } from '../types/api';

// Sibling of `useFleetSummary` for the site/topology widgets (site-matrix, region-rollup, geo-map).
// Same dedupe contract, separate module-level state — so it gets its own coverage rather than
// relying on the sibling's.

const getFleetGroupSummary = vi.fn();
vi.mock('../services/api', () => ({
  api: { getFleetGroupSummary: () => getFleetGroupSummary() },
}));

const summary = () => ({ groups: [{ group_id: 'g1', name: 'tokyo', ok: 4, critical: 1 }] }) as unknown as FleetGroupSummary;

describe('useGroupSummary', () => {
  beforeEach(() => {
    vi.resetModules();
    vi.useFakeTimers({ shouldAdvanceTime: true });
    getFleetGroupSummary.mockReset().mockResolvedValue(summary());
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('fetches once and shares it across consumers', async () => {
    const { useGroupSummary } = await import('./useGroupSummary');
    const a = renderHook(() => useGroupSummary());
    const b = renderHook(() => useGroupSummary());

    await waitFor(() => expect(a.result.current.summary).not.toBeNull());
    expect(getFleetGroupSummary).toHaveBeenCalledTimes(1);
    expect(b.result.current.summary).toEqual(a.result.current.summary);
  });

  it('polls on a single shared 15s timer and stops with the last consumer', async () => {
    const { useGroupSummary } = await import('./useGroupSummary');
    const a = renderHook(() => useGroupSummary());
    const b = renderHook(() => useGroupSummary());
    await waitFor(() => expect(getFleetGroupSummary).toHaveBeenCalledTimes(1));

    await vi.advanceTimersByTimeAsync(15_000);
    await waitFor(() => expect(getFleetGroupSummary).toHaveBeenCalledTimes(2));

    a.unmount();
    b.unmount();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(getFleetGroupSummary).toHaveBeenCalledTimes(2);
  });

  it('drops ticks that land while a request is still in flight', async () => {
    let settle: (v: FleetGroupSummary) => void = () => {};
    getFleetGroupSummary.mockImplementation(
      () =>
        new Promise<FleetGroupSummary>((res) => {
          settle = res;
        }),
    );
    const { useGroupSummary } = await import('./useGroupSummary');
    renderHook(() => useGroupSummary());
    await waitFor(() => expect(getFleetGroupSummary).toHaveBeenCalledTimes(1));

    await vi.advanceTimersByTimeAsync(45_000);
    expect(getFleetGroupSummary).toHaveBeenCalledTimes(1);
    settle(summary());
  });

  it('surfaces a failure as a flag rather than an exception', async () => {
    getFleetGroupSummary.mockRejectedValueOnce(new Error('down'));
    const { useGroupSummary } = await import('./useGroupSummary');
    const { result } = renderHook(() => useGroupSummary());

    await waitFor(() => expect(result.current.error).toBe(true));
    expect(result.current.loading).toBe(false);
  });
});
