// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
// The wiring half of the shared loader. What an answer does to the screen is `loadReducer`, tested
// in `loadState.test.ts` without a DOM; this pins when a read is made at all.
import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '../services/api';
import { useConfigChangeStore } from './configChanges';
import { useLoad } from './useLoad';

function setHidden(hidden: boolean) {
  Object.defineProperty(document, 'visibilityState', {
    value: hidden ? 'hidden' : 'visible',
    configurable: true,
  });
  document.dispatchEvent(new Event('visibilitychange'));
}

/** A promise the test settles by hand, to put answers in the order it wants. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

describe('useLoad', () => {
  beforeEach(() => setHidden(false));
  afterEach(() => {
    setHidden(false);
    vi.useRealTimers();
  });

  it('reads on mount, and reload replaces the rows without flashing loading', async () => {
    const fetch = vi.fn().mockResolvedValueOnce(['a']).mockResolvedValueOnce(['a', 'b']);
    const { result } = renderHook(() => useLoad(fetch, [], { initial: [] as string[] }));
    expect(result.current.loading).toBe(true);
    await waitFor(() => expect(result.current.data).toEqual(['a']));
    expect(result.current.loading).toBe(false);

    act(() => result.current.reload());
    expect(result.current.loading).toBe(false);
    await waitFor(() => expect(result.current.data).toEqual(['a', 'b']));
  });

  it('keeps the same data object until an answer replaces it', async () => {
    const rows = ['a'];
    const { result, rerender } = renderHook(() =>
      useLoad(() => Promise.resolve(rows), [], { initial: [] as string[] }),
    );
    await waitFor(() => expect(result.current.data).toBe(rows));
    const before = result.current.data;
    rerender();
    expect(result.current.data).toBe(before);
  });

  it('a dependency change re-reads, and the answer to the older question never lands', async () => {
    const first = deferred<string>();
    const second = deferred<string>();
    const fetch = vi.fn((q: string) => (q === 'old' ? first.promise : second.promise));
    const { result, rerender } = renderHook(
      ({ q }) => useLoad(() => fetch(q), [q], { initial: '' }),
      { initialProps: { q: 'old' } },
    );
    rerender({ q: 'new' });
    await act(async () => second.resolve('new rows'));
    await act(async () => first.resolve('old rows'));
    expect(result.current.data).toBe('new rows');
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('names a refusal and keeps nothing it did not have', async () => {
    const { result } = renderHook(() =>
      useLoad(() => Promise.reject(new ApiError('forbidden', 'no', 403)), [], { initial: [] }),
    );
    await waitFor(() => expect(result.current.block).toBe('forbidden'));
    expect(result.current.data).toEqual([]);
  });

  it('does not ask while disabled, and settles loading', async () => {
    const fetch = vi.fn().mockResolvedValue(1);
    const { result, rerender } = renderHook(
      ({ on }) => useLoad(fetch, [], { initial: 0, enabled: on }),
      { initialProps: { on: false } },
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(fetch).not.toHaveBeenCalled();
    rerender({ on: true });
    await waitFor(() => expect(result.current.data).toBe(1));
  });

  it('re-reads on a configuration change only when asked to', async () => {
    const quiet = vi.fn().mockResolvedValue(1);
    const told = vi.fn().mockResolvedValue(1);
    renderHook(() => useLoad(quiet, [], { initial: 0 }));
    renderHook(() => useLoad(told, [], { initial: 0, onConfigChange: true }));
    await waitFor(() => expect(told).toHaveBeenCalledTimes(1));

    act(() => useConfigChangeStore.setState((s) => ({ ...s, changes: s.changes + 1 })));
    await waitFor(() => expect(told).toHaveBeenCalledTimes(2));
    expect(quiet).toHaveBeenCalledTimes(1);
  });

  it('polls on intervalMs while the tab is visible, and reads at once on return', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const fetch = vi.fn().mockResolvedValue(1);
    renderHook(() => useLoad(fetch, [], { initial: 0, intervalMs: 10_000 }));
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
    await vi.advanceTimersByTimeAsync(10_000);
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(2));

    act(() => setHidden(true));
    await vi.advanceTimersByTimeAsync(40_000);
    expect(fetch).toHaveBeenCalledTimes(2);

    act(() => setHidden(false));
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(3));
  });
});
