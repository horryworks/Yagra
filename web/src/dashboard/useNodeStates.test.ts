// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { NodeState } from '../types/api';

// One shared SSE subscription feeds every live view, so the two behaviours that matter are the
// ref-count (exactly one connection no matter how many widgets mount) and the coalescing flush
// (a post-restart full-sweep burst is tens of thousands of events — it must cost O(nodes) per
// flush, not one re-render per event).

const unsubscribe = vi.fn();
let emit: (ev: { node_id: string; state: NodeState }) => void = () => {};
const subscribeNodeStates = vi.fn((cb: (ev: { node_id: string; state: NodeState }) => void) => {
  emit = cb;
  return unsubscribe;
});

vi.mock('../services/sse', () => ({
  subscribeNodeStates: (cb: (ev: { node_id: string; state: NodeState }) => void) =>
    subscribeNodeStates(cb),
}));

const load = async () => await import('./useNodeStates');

describe('useNodeStates', () => {
  beforeEach(async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const m = await load();
    m.resetNodeStates();
    subscribeNodeStates.mockClear();
    unsubscribe.mockClear();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('opens exactly one subscription however many consumers mount', async () => {
    const { useNodeStates } = await load();
    const a = renderHook(() => useNodeStates());
    const b = renderHook(() => useNodeStates());
    const c = renderHook(() => useNodeStates());

    expect(subscribeNodeStates).toHaveBeenCalledTimes(1);

    // The connection stays open until the *last* consumer leaves.
    a.unmount();
    b.unmount();
    expect(unsubscribe).not.toHaveBeenCalled();
    c.unmount();
    expect(unsubscribe).toHaveBeenCalledTimes(1);
  });

  it('re-subscribes after the last consumer left and a new one arrives', async () => {
    const { useNodeStates } = await load();
    renderHook(() => useNodeStates()).unmount();
    expect(unsubscribe).toHaveBeenCalledTimes(1);

    renderHook(() => useNodeStates());
    expect(subscribeNodeStates).toHaveBeenCalledTimes(2);
  });

  it('publishes buffered events as one batch on flush', async () => {
    const { useNodeStates } = await load();
    const { result } = renderHook(() => useNodeStates());

    emit({ node_id: 'n1', state: 'critical' });
    emit({ node_id: 'n2', state: 'warning' });
    // Not visible yet — events are buffered until the flush timer fires.
    expect(result.current.size).toBe(0);

    await vi.advanceTimersByTimeAsync(150);
    await waitFor(() => expect(result.current.size).toBe(2));
    expect(result.current.get('n1')).toBe('critical');
    expect(result.current.get('n2')).toBe('warning');
  });

  it('publishes a fresh Map identity so memoized consumers recompute', async () => {
    const { useNodeStates } = await load();
    const { result } = renderHook(() => useNodeStates());
    const before = result.current;

    emit({ node_id: 'n1', state: 'ok' });
    await vi.advanceTimersByTimeAsync(150);

    await waitFor(() => expect(result.current).not.toBe(before));
  });

  it('does not republish when a flush changes nothing', async () => {
    const { useNodeStates } = await load();
    const { result } = renderHook(() => useNodeStates());

    emit({ node_id: 'n1', state: 'ok' });
    await vi.advanceTimersByTimeAsync(150);
    await waitFor(() => expect(result.current.get('n1')).toBe('ok'));
    const settled = result.current;

    // Same node, same state ⇒ no change ⇒ the Map identity must be preserved, or every
    // `useMemo` keyed on it would recompute for nothing on each keepalive sweep.
    emit({ node_id: 'n1', state: 'ok' });
    await vi.advanceTimersByTimeAsync(150);
    expect(result.current).toBe(settled);
  });

  it('keeps only the newest state when a node flaps within one flush window', async () => {
    const { useNodeStates } = await load();
    const { result } = renderHook(() => useNodeStates());

    emit({ node_id: 'n1', state: 'ok' });
    emit({ node_id: 'n1', state: 'warning' });
    emit({ node_id: 'n1', state: 'critical' });
    await vi.advanceTimersByTimeAsync(150);

    await waitFor(() => expect(result.current.get('n1')).toBe('critical'));
    expect(result.current.size).toBe(1);
  });

  it('coalesces a large burst into a single published Map', async () => {
    const { useNodeStates, ingestNodeState } = await load();
    const { result } = renderHook(() => useNodeStates());

    for (let i = 0; i < 5_000; i += 1) ingestNodeState(`n${i}`, 'ok');
    expect(result.current.size).toBe(0);

    await vi.advanceTimersByTimeAsync(150);
    await waitFor(() => expect(result.current.size).toBe(5_000));
  });

  it('resetNodeStates clears both the map and the pending buffer', async () => {
    const { useNodeStates, ingestNodeState, resetNodeStates } = await load();
    const { result } = renderHook(() => useNodeStates());

    ingestNodeState('n1', 'critical');
    resetNodeStates();
    await vi.advanceTimersByTimeAsync(150);

    expect(result.current.size).toBe(0);
  });
});
