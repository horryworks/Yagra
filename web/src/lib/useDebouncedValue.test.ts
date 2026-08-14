// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
//
// The one debounce, under test — and the file it tests used to say it could not be. That claim was
// true when it was written and is not now: twelve hook tests carry the `@vitest-environment jsdom`
// pragma above, so `renderHook` is reachable from a plain `.ts`. The ban `testing.md` states is on
// `.tsx` test files (Vitest's `include` never matches them), not on hooks.
//
// Why it is worth the file rather than trusting eight callers: every property here is invisible in a
// screen test and expensive when wrong. A debounce that re-arms on the wrong dependency fires for
// the term *before* the last keystroke — which looks like the box being laggy, not broken. And the
// `ms = 0` path is what `useNodeSearch` uses for "the box was cleared"; if that settled
// synchronously it would be a second code path, which is exactly what the parameter exists to avoid.

import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { SEARCH_DEBOUNCE_MS, useDebouncedValue } from './useDebouncedValue';

/** Step the timers inside `act`, so the resulting state update is flushed before we assert. */
const tick = async (ms: number) => {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
};

describe('useDebouncedValue', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('publishes its initial value with no wait', () => {
    // A filter restored from a URL must render narrowed on the first paint. Seeding `useState` with
    // the value rather than with a blank is what makes that true, and it is the reason
    // `useFilterSearch.test.ts` can say "a term present at mount is NOT debounced".
    const { result } = renderHook(() => useDebouncedValue('sw-core'));
    expect(result.current).toBe('sw-core');
  });

  it('settles once, on the last value of a burst', async () => {
    const { result, rerender } = renderHook((v: string) => useDebouncedValue(v), {
      initialProps: 'a',
    });
    rerender('ab');
    rerender('abc');
    rerender('abcd');

    await tick(SEARCH_DEBOUNCE_MS - 1);
    expect(result.current).toBe('a'); // the burst has not settled — nothing intermediate escaped

    await tick(1);
    expect(result.current).toBe('abcd');
  });

  it('re-arms on a changed delay, not only on a changed value', async () => {
    // The effect depends on both. If it depended on `value` alone, the pending timer would keep the
    // OLD delay — so a caller switching to a longer wait mid-burst would settle early, and the
    // "settle immediately when the box is cleared" idiom in the doc comment would be a coin flip.
    const { result, rerender } = renderHook(
      ({ v, ms }: { v: string; ms: number }) => useDebouncedValue(v, ms),
      { initialProps: { v: 'a', ms: 200 } },
    );
    rerender({ v: 'b', ms: 200 });
    await tick(150);
    expect(result.current).toBe('a');

    // 1000ms from *here*. Without a re-arm the original timer fires 50ms from now.
    rerender({ v: 'b', ms: 1000 });
    await tick(50);
    expect(result.current).toBe('a');

    await tick(950);
    expect(result.current).toBe('b');
  });

  it('settles on the next tick when the delay is zero, never synchronously', async () => {
    // `useNodeSearch` clears its box with `ms = 0`. Settling synchronously would mean a render in
    // which the value changed without an effect running — one extra code path for the callers to
    // reason about, which is the whole thing the parameter was added to avoid.
    const { result, rerender } = renderHook((v: string) => useDebouncedValue(v, 0), {
      initialProps: 'a',
    });
    rerender('');
    expect(result.current).toBe('a');

    await tick(0);
    expect(result.current).toBe('');
  });

  it('debounces a value of any type, not just a string', async () => {
    // Generic because `NodePicker` settles a term while `CollectionEditor` settles a draft object.
    const first = { id: 1 };
    const second = { id: 2 };
    const { result, rerender } = renderHook((v: { id: number }) => useDebouncedValue(v), {
      initialProps: first,
    });
    rerender(second);
    await tick(SEARCH_DEBOUNCE_MS);
    expect(result.current).toBe(second);
  });
});
