// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

// The change feed's client half (ADR-019 増分 2). What must hold: the first revision is not a
// change (every open screen has just read everything), a reconnect that finds the same revision is
// not a change (Tier1's mock stream ends every 3 s, and a reload per reconnect would thrash every
// screen), and a screen reacts only to changes heard after it mounted.

let onRevision: (rev: number) => void = () => {};
const unsubscribe = vi.fn();
vi.mock('../services/sse', () => ({
  subscribeConfigChanges: (cb: (rev: number) => void) => {
    onRevision = cb;
    return unsubscribe;
  },
}));

describe('nextConfigState', () => {
  it('remembers the first revision without counting it', async () => {
    const { nextConfigState } = await import('./configChanges');
    expect(nextConfigState({ revision: null, changes: 0 }, 41)).toEqual({ revision: 41, changes: 0 });
  });

  it('does not count the same revision again (a reconnect that missed nothing)', async () => {
    const { nextConfigState } = await import('./configChanges');
    const s = { revision: 41, changes: 3 };
    expect(nextConfigState(s, 41)).toBe(s);
  });

  it('counts any different revision — including a smaller one from a restarted core', async () => {
    const { nextConfigState } = await import('./configChanges');
    expect(nextConfigState({ revision: 41, changes: 3 }, 42)).toEqual({ revision: 42, changes: 4 });
    expect(nextConfigState({ revision: 41, changes: 4 }, 0)).toEqual({ revision: 0, changes: 5 });
  });
});

describe('the stream and a screen that follows it', () => {
  beforeEach(async () => {
    vi.clearAllMocks();
    const { useConfigChangeStore } = await import('./configChanges');
    useConfigChangeStore.setState({ revision: null, changes: 0 });
  });

  it('subscribes once and closes on unmount', async () => {
    const { useConfigChangeStream } = await import('./configChanges');
    const { unmount } = renderHook(() => useConfigChangeStream());
    unmount();
    expect(unsubscribe).toHaveBeenCalledTimes(1);
  });

  it('runs the callback for a change heard after mount, once per change', async () => {
    const { useConfigChangeStream, useOnConfigChange } = await import('./configChanges');
    renderHook(() => useConfigChangeStream());
    act(() => onRevision(10)); // the first frame on connect
    const cb = vi.fn();
    const { rerender } = renderHook(({ f }) => useOnConfigChange(f), { initialProps: { f: cb } });
    expect(cb).not.toHaveBeenCalled();

    act(() => onRevision(10)); // a reconnect that missed nothing
    expect(cb).not.toHaveBeenCalled();

    act(() => onRevision(11));
    expect(cb).toHaveBeenCalledTimes(1);

    // A new callback identity is not a new change.
    const cb2 = vi.fn();
    rerender({ f: cb2 });
    expect(cb2).not.toHaveBeenCalled();
    act(() => onRevision(12));
    expect(cb2).toHaveBeenCalledTimes(1);
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('does not replay changes heard before the screen mounted', async () => {
    const { useConfigChangeStream, useOnConfigChange } = await import('./configChanges');
    renderHook(() => useConfigChangeStream());
    act(() => onRevision(1));
    act(() => onRevision(2));
    act(() => onRevision(3));
    const cb = vi.fn();
    renderHook(() => useOnConfigChange(cb));
    expect(cb).not.toHaveBeenCalled();
  });
});
