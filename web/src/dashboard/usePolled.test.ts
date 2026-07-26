// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '../services/api';
import { usePolled } from './usePolled';

// `usePolled` backs every snapshot-reading dashboard widget, so its contract is load-bearing:
// fetch on mount, re-fetch on the interval, surface a readable error, and never write into a
// torn-down widget (which in React would be a state-update-after-unmount leak).

describe('usePolled', () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('starts loading, then publishes the first result', async () => {
    const fetcher = vi.fn().mockResolvedValue({ n: 1 });
    const { result } = renderHook(() => usePolled(fetcher));

    expect(result.current).toEqual({ data: null, loading: true, error: null });
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current).toEqual({ data: { n: 1 }, loading: false, error: null });
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it('re-fetches on each interval tick', async () => {
    let n = 0;
    const fetcher = vi.fn().mockImplementation(() => Promise.resolve({ n: ++n }));
    const { result } = renderHook(() => usePolled(fetcher, [], 5_000));

    await waitFor(() => expect(result.current.data).toEqual({ n: 1 }));
    await vi.advanceTimersByTimeAsync(5_000);
    await waitFor(() => expect(result.current.data).toEqual({ n: 2 }));
    await vi.advanceTimersByTimeAsync(5_000);
    await waitFor(() => expect(result.current.data).toEqual({ n: 3 }));
  });

  it('surfaces an ApiError message verbatim so the widget can show the server reason', async () => {
    const fetcher = vi.fn().mockRejectedValue(new ApiError('tier_off', 'flow tier disabled', 503));
    const { result } = renderHook(() => usePolled(fetcher));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.error).toBe('flow tier disabled');
  });

  it('falls back to a generic message for a non-ApiError failure', async () => {
    const fetcher = vi.fn().mockRejectedValue(new TypeError('NetworkError: fetch failed'));
    const { result } = renderHook(() => usePolled(fetcher));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.error).toBeTruthy();
    // A raw transport/exception string must not be shown to the operator as-is.
    expect(result.current.error).not.toContain('NetworkError');
  });

  it('keeps the last good data when a later poll fails', async () => {
    const fetcher = vi
      .fn()
      .mockResolvedValueOnce({ n: 1 })
      .mockRejectedValue(new ApiError('boom', 'upstream down', 500));
    const { result } = renderHook(() => usePolled(fetcher, [], 5_000));

    await waitFor(() => expect(result.current.data).toEqual({ n: 1 }));
    await vi.advanceTimersByTimeAsync(5_000);
    await waitFor(() => expect(result.current.error).toBe('upstream down'));
    // Stale-but-known beats blanking the widget.
    expect(result.current.data).toEqual({ n: 1 });
  });

  it('stops polling on unmount', async () => {
    const fetcher = vi.fn().mockResolvedValue({ n: 1 });
    const { unmount, result } = renderHook(() => usePolled(fetcher, [], 5_000));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(fetcher).toHaveBeenCalledTimes(1);

    unmount();
    await vi.advanceTimersByTimeAsync(20_000);
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it('ignores a response that lands after unmount', async () => {
    let settle: (v: unknown) => void = () => {};
    const fetcher = vi.fn().mockImplementation(
      () =>
        new Promise((res) => {
          settle = res;
        }),
    );
    const { unmount } = renderHook(() => usePolled(fetcher, [], 5_000));
    unmount();

    // Resolving a request that was already in flight at unmount must be a no-op, not a state
    // update on a torn-down component.
    expect(() => settle({ n: 9 })).not.toThrow();
    await vi.advanceTimersByTimeAsync(0);
  });

  it('re-arms when deps change', async () => {
    const fetcher = vi.fn().mockResolvedValue({ n: 1 });
    const { rerender, result } = renderHook(({ dep }) => usePolled(fetcher, [dep], 5_000), {
      initialProps: { dep: 'a' },
    });

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(fetcher).toHaveBeenCalledTimes(1);

    rerender({ dep: 'b' });
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(2));
  });
});
