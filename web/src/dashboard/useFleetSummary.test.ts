// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { FleetSummary } from '../types/api';

// The point of this hook is deduplication: several status widgets mount at once and must produce
// ONE `/fleet/summary` request and ONE 15s timer between them (S12). A regression here multiplies
// dashboard load by the widget count, which is exactly the shape of bug that only shows at scale.

const getFleetSummary = vi.fn();
vi.mock('../services/api', () => ({
  api: { getFleetSummary: () => getFleetSummary() },
}));

const summary = (over: Partial<FleetSummary> = {}): FleetSummary =>
  ({ total: 3, ok: 2, warning: 1, critical: 0, unknown: 0, unreachable: 0, maintenance: 0, ...over }) as FleetSummary;

describe('useFleetSummary', () => {
  beforeEach(() => {
    vi.resetModules();
    vi.useFakeTimers({ shouldAdvanceTime: true });
    getFleetSummary.mockReset().mockResolvedValue(summary());
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('fetches once and shares the result across every mounted consumer', async () => {
    const { useFleetSummary } = await import('./useFleetSummary');
    const a = renderHook(() => useFleetSummary());
    const b = renderHook(() => useFleetSummary());

    await waitFor(() => expect(a.result.current.summary).not.toBeNull());
    expect(getFleetSummary).toHaveBeenCalledTimes(1);
    expect(b.result.current.summary).toEqual(a.result.current.summary);
    expect(a.result.current.loading).toBe(false);
    expect(a.result.current.error).toBe(false);
  });

  it('runs one shared 15s poll, not one per consumer', async () => {
    const { useFleetSummary } = await import('./useFleetSummary');
    renderHook(() => useFleetSummary());
    renderHook(() => useFleetSummary());
    await waitFor(() => expect(getFleetSummary).toHaveBeenCalledTimes(1));

    await vi.advanceTimersByTimeAsync(15_000);
    await waitFor(() => expect(getFleetSummary).toHaveBeenCalledTimes(2));
    await vi.advanceTimersByTimeAsync(15_000);
    await waitFor(() => expect(getFleetSummary).toHaveBeenCalledTimes(3));
  });

  it('stops polling once the last consumer unmounts', async () => {
    const { useFleetSummary } = await import('./useFleetSummary');
    const a = renderHook(() => useFleetSummary());
    const b = renderHook(() => useFleetSummary());
    await waitFor(() => expect(getFleetSummary).toHaveBeenCalledTimes(1));

    a.unmount();
    await vi.advanceTimersByTimeAsync(15_000);
    await waitFor(() => expect(getFleetSummary).toHaveBeenCalledTimes(2));

    b.unmount();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(getFleetSummary).toHaveBeenCalledTimes(2);
  });

  it('does not stack overlapping requests when one is still in flight', async () => {
    let settle: (v: FleetSummary) => void = () => {};
    getFleetSummary.mockImplementation(
      () =>
        new Promise<FleetSummary>((res) => {
          settle = res;
        }),
    );
    const { useFleetSummary } = await import('./useFleetSummary');
    renderHook(() => useFleetSummary());
    await waitFor(() => expect(getFleetSummary).toHaveBeenCalledTimes(1));

    // Ticks that arrive while the first request is outstanding are dropped, not queued —
    // otherwise a slow core would accumulate a backlog of identical requests.
    await vi.advanceTimersByTimeAsync(45_000);
    expect(getFleetSummary).toHaveBeenCalledTimes(1);

    settle(summary());
    await vi.advanceTimersByTimeAsync(15_000);
    await waitFor(() => expect(getFleetSummary).toHaveBeenCalledTimes(2));
  });

  it('flags an error without throwing, and recovers on the next successful poll', async () => {
    getFleetSummary.mockRejectedValueOnce(new Error('down'));
    const { useFleetSummary } = await import('./useFleetSummary');
    const { result } = renderHook(() => useFleetSummary());

    await waitFor(() => expect(result.current.error).toBe(true));
    expect(result.current.loading).toBe(false);

    await vi.advanceTimersByTimeAsync(15_000);
    await waitFor(() => expect(result.current.error).toBe(false));
    expect(result.current.summary).not.toBeNull();
  });
});
