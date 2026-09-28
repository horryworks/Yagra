// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
// The dedupe itself (one request, one timer, in-flight drop, stop on last unmount) is pinned by
// `dashboard/useFleetSummary.test.ts` and `useGroupSummary.test.ts`, which predate this module and
// pass against it unchanged. This file covers what the move added — the hidden tab — and the guard.
import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { codeOnly, readSources } from '../testSupport/sources';
import { createSharedPoll, POLL_INTERVAL_MS } from './sharedPoll';

function setHidden(hidden: boolean) {
  Object.defineProperty(document, 'visibilityState', {
    value: hidden ? 'hidden' : 'visible',
    configurable: true,
  });
  document.dispatchEvent(new Event('visibilitychange'));
}

describe('createSharedPoll', () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    setHidden(false);
  });
  afterEach(() => {
    setHidden(false);
    vi.useRealTimers();
  });

  it('stops while the tab is hidden and reads at once on return', async () => {
    const fetch = vi.fn().mockResolvedValue(1);
    const usePoll = createSharedPoll(fetch);
    renderHook(() => usePoll());
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));

    act(() => setHidden(true));
    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS * 4);
    expect(fetch).toHaveBeenCalledTimes(1);

    act(() => setHidden(false));
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(2));
    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS);
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(3));
  });

  it('keeps the last answer through a failed read', async () => {
    const fetch = vi.fn().mockResolvedValueOnce('first').mockRejectedValueOnce(new Error('down'));
    const usePoll = createSharedPoll(fetch);
    const { result } = renderHook(() => usePoll());
    await waitFor(() => expect(result.current.data).toBe('first'));

    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS);
    await waitFor(() => expect(result.current.error).toBe(true));
    expect(result.current.data).toBe('first');
  });

  it('a hidden tab mounts without reading, and reads when shown', async () => {
    setHidden(true);
    const fetch = vi.fn().mockResolvedValue(1);
    const usePoll = createSharedPoll(fetch);
    renderHook(() => usePoll());
    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS * 2);
    expect(fetch).not.toHaveBeenCalled();
    act(() => setHidden(false));
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
  });
});

/**
 * ADR-184: the refresh cadence is written once. `useFleetSummary`, `useGroupSummary`, `usePolled`,
 * `refreshTick` and System health each declared their own `15_000`.
 *
 * ⚠️ Assembled at runtime, or it would match this file.
 */
describe('the refresh cadence is POLL_INTERVAL_MS', () => {
  /** Where the number may appear in code, and why it is not the cadence there. */
  const DECLARED: Record<string, string> = {
    'lib/sharedPoll.ts': 'the declaration',
    'dashboard/widgets/util.ts': "a chart's query step for a range of an hour or less — a resolution, not a refresh",
  };
  const NEEDLE = new RegExp(`\\b15${'_?'}000\\b`);

  it('no other module declares the 15-second interval', () => {
    const files = readSources();
    expect(files.length).toBeGreaterThan(300);
    const offenders = files
      .filter(([p, src]) => !(p in DECLARED) && NEEDLE.test(codeOnly(src)))
      .map(([p]) => p);
    expect(offenders, 'import POLL_INTERVAL_MS from lib/sharedPoll').toEqual([]);
    for (const p of Object.keys(DECLARED)) {
      expect(NEEDLE.test(codeOnly(files.find(([f]) => f === p)?.[1] ?? '')), p).toBe(true);
    }
  });
});
