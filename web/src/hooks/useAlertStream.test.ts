// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Alert } from '../types/api';

// Seed-then-stream (ADR-019). The failure modes worth pinning: a gated/transient seed fetch must
// NOT prevent the SSE subscription (otherwise a viewer who can't list alerts gets no live updates
// either), unmount must actually close the stream, and (増分 1) the seed REPLACES the store and runs
// again on every resync — an alert resolved while the stream was down must drop out — while a
// failed re-seed leaves the store alone rather than zeroing the bell.

const listAlerts = vi.fn();
const unsubscribe = vi.fn();
let onUpsert: (a: Alert) => void = () => {};
let onResolve: (a: Alert) => void = () => {};
let onResync: (() => void) | undefined;
const subscribeAlerts = vi.fn(
  (up: (a: Alert) => void, res: (a: Alert) => void, _err?: unknown, resync?: () => void) => {
    onUpsert = up;
    onResolve = res;
    onResync = resync;
    return unsubscribe;
  },
);

const upsertAlert = vi.fn();
const resolveAlert = vi.fn();
const setAlerts = vi.fn();
const clear = vi.fn();

vi.mock('../services/api', () => ({ api: { listAlerts: () => listAlerts() } }));
vi.mock('../services/sse', () => ({
  subscribeAlerts: (
    up: (a: Alert) => void,
    res: (a: Alert) => void,
    err?: unknown,
    resync?: () => void,
  ) => subscribeAlerts(up, res, err, resync),
}));
vi.mock('../store', () => ({
  useAlertStore: (sel: (s: unknown) => unknown) =>
    sel({ upsertAlert, resolveAlert, setAlerts, clear }),
}));

const alert = (id: string): Alert => ({ id, severity: 'critical' }) as unknown as Alert;

describe('useAlertStream', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    onResync = undefined;
    listAlerts.mockResolvedValue([alert('a1'), alert('a2')]);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('replaces the store with the snapshot and opens the stream', async () => {
    const { useAlertStream } = await import('./useAlertStream');
    renderHook(() => useAlertStream());

    // One wholesale replace, not one upsert per row: an upsert-only seed is what kept a
    // resolution missed during a disconnect on screen until a reload.
    await waitFor(() => expect(setAlerts).toHaveBeenCalledWith([alert('a1'), alert('a2')]));
    expect(setAlerts).toHaveBeenCalledTimes(1);
    expect(upsertAlert).not.toHaveBeenCalled();
    expect(subscribeAlerts).toHaveBeenCalledTimes(1);
  });

  it('still subscribes when the seed fetch fails, and leaves the store alone', async () => {
    listAlerts.mockRejectedValue(new Error('403 gated'));
    const { useAlertStream } = await import('./useAlertStream');
    renderHook(() => useAlertStream());

    // The rejection is swallowed on purpose — live SSE is the fallback path, so the subscription
    // must be established regardless.
    await waitFor(() => expect(subscribeAlerts).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(listAlerts).toHaveBeenCalledTimes(1));
    await Promise.resolve();
    expect(setAlerts).not.toHaveBeenCalled();
    expect(clear).not.toHaveBeenCalled();
  });

  it('re-reads the snapshot on a resync and replaces the store with it', async () => {
    const { useAlertStream } = await import('./useAlertStream');
    renderHook(() => useAlertStream());
    await waitFor(() => expect(setAlerts).toHaveBeenCalledTimes(1));
    expect(onResync).toBeTypeOf('function');

    // a2 was resolved while the stream was down; the fresh snapshot no longer has it.
    listAlerts.mockResolvedValue([alert('a1')]);
    onResync!();
    await waitFor(() => expect(setAlerts).toHaveBeenCalledTimes(2));
    expect(setAlerts).toHaveBeenLastCalledWith([alert('a1')]);
  });

  it('a failed re-seed keeps what the store has (the bell does not drop to zero)', async () => {
    const { useAlertStream } = await import('./useAlertStream');
    renderHook(() => useAlertStream());
    await waitFor(() => expect(setAlerts).toHaveBeenCalledTimes(1));

    listAlerts.mockRejectedValue(new Error('network'));
    onResync!();
    await waitFor(() => expect(listAlerts).toHaveBeenCalledTimes(2));
    await Promise.resolve();
    expect(setAlerts).toHaveBeenCalledTimes(1);
    expect(clear).not.toHaveBeenCalled();
  });

  it('routes stream events to the matching store action', async () => {
    const { useAlertStream } = await import('./useAlertStream');
    renderHook(() => useAlertStream());
    await waitFor(() => expect(subscribeAlerts).toHaveBeenCalled());

    onUpsert(alert('live-1'));
    expect(upsertAlert).toHaveBeenCalledWith(alert('live-1'));

    onResolve(alert('live-1'));
    expect(resolveAlert).toHaveBeenCalledWith(alert('live-1'));
  });

  it('does nothing while disabled (the public board without an alert widget)', async () => {
    const { useAlertStream } = await import('./useAlertStream');
    renderHook(() => useAlertStream(false));
    await Promise.resolve();
    expect(listAlerts).not.toHaveBeenCalled();
    expect(subscribeAlerts).not.toHaveBeenCalled();
  });

  it('closes the stream on unmount', async () => {
    const { useAlertStream } = await import('./useAlertStream');
    const { unmount } = renderHook(() => useAlertStream());
    await waitFor(() => expect(subscribeAlerts).toHaveBeenCalled());

    unmount();
    expect(unsubscribe).toHaveBeenCalledTimes(1);
  });
});
