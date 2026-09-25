// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { SYNC_WATCH_POLL_MS, useSyncWatch } from './useSyncWatch';

// The integration pages lean on this for "keep moving while a read runs, reload once when it ends".
// Getting the edge wrong either leaves the progress frozen or reloads thousands of device rows on
// every render.

describe('useSyncWatch', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('polls on the interval while a read is active, and not before', () => {
    const poll = vi.fn();
    const settled = vi.fn();
    renderHook(() => useSyncWatch(true, poll, settled));

    expect(poll).not.toHaveBeenCalled();
    vi.advanceTimersByTime(SYNC_WATCH_POLL_MS);
    expect(poll).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(SYNC_WATCH_POLL_MS * 2);
    expect(poll).toHaveBeenCalledTimes(3);
    expect(settled).not.toHaveBeenCalled();
  });

  it('neither polls nor settles when no read was ever running', () => {
    const poll = vi.fn();
    const settled = vi.fn();
    const { rerender } = renderHook(({ on }) => useSyncWatch(on, poll, settled), {
      initialProps: { on: false },
    });

    vi.advanceTimersByTime(SYNC_WATCH_POLL_MS * 3);
    rerender({ on: false });
    expect(poll).not.toHaveBeenCalled();
    expect(settled).not.toHaveBeenCalled();
  });

  it('settles exactly once when the read ends, and stops polling', () => {
    const poll = vi.fn();
    const settled = vi.fn();
    const { rerender } = renderHook(({ on }) => useSyncWatch(on, poll, settled), {
      initialProps: { on: true },
    });

    rerender({ on: false });
    expect(settled).toHaveBeenCalledTimes(1);

    // A later render with the read still over is not a second end.
    rerender({ on: false });
    expect(settled).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(SYNC_WATCH_POLL_MS * 3);
    expect(poll).not.toHaveBeenCalled();
  });

  it('settles again after a second read starts and ends', () => {
    const poll = vi.fn();
    const settled = vi.fn();
    const { rerender } = renderHook(({ on }) => useSyncWatch(on, poll, settled), {
      initialProps: { on: true },
    });

    rerender({ on: false });
    rerender({ on: true });
    rerender({ on: false });
    expect(settled).toHaveBeenCalledTimes(2);
  });
});
