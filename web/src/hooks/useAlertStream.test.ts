// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Alert } from '../types/api';

// Seed-then-stream (ADR-019). The two failure modes worth pinning: a gated/transient seed fetch
// must NOT prevent the SSE subscription (otherwise a viewer who can't list alerts gets no live
// updates either), and unmount must actually close the stream.

const listAlerts = vi.fn();
const unsubscribe = vi.fn();
let onUpsert: (a: Alert) => void = () => {};
let onResolve: (a: Alert) => void = () => {};
const subscribeAlerts = vi.fn((up: (a: Alert) => void, res: (a: Alert) => void) => {
  onUpsert = up;
  onResolve = res;
  return unsubscribe;
});

const upsertAlert = vi.fn();
const resolveAlert = vi.fn();

vi.mock('../services/api', () => ({ api: { listAlerts: () => listAlerts() } }));
vi.mock('../services/sse', () => ({
  subscribeAlerts: (up: (a: Alert) => void, res: (a: Alert) => void) => subscribeAlerts(up, res),
}));
vi.mock('../store', () => ({
  useAlertStore: (sel: (s: unknown) => unknown) => sel({ upsertAlert, resolveAlert }),
}));

const alert = (id: string): Alert => ({ id, severity: 'critical' }) as unknown as Alert;

describe('useAlertStream', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    listAlerts.mockResolvedValue([alert('a1'), alert('a2')]);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('seeds the store from the snapshot and opens the stream', async () => {
    const { useAlertStream } = await import('./useAlertStream');
    renderHook(() => useAlertStream());

    await waitFor(() => expect(upsertAlert).toHaveBeenCalledTimes(2));
    // Asserted on the first argument only: the seed is `list.forEach(upsertAlert)`, so each call
    // also carries forEach's index + array. Harmless for a 1-arg store action, but it means the
    // action must never grow a meaningful second parameter.
    expect(upsertAlert.mock.calls.map((c) => c[0])).toEqual([alert('a1'), alert('a2')]);
    expect(subscribeAlerts).toHaveBeenCalledTimes(1);
  });

  it('still subscribes when the seed fetch fails', async () => {
    listAlerts.mockRejectedValue(new Error('403 gated'));
    const { useAlertStream } = await import('./useAlertStream');
    renderHook(() => useAlertStream());

    // The rejection is swallowed on purpose — live SSE is the fallback path, so the subscription
    // must be established regardless.
    await waitFor(() => expect(subscribeAlerts).toHaveBeenCalledTimes(1));
    expect(upsertAlert).not.toHaveBeenCalled();
  });

  it('routes stream events to the matching store action', async () => {
    const { useAlertStream } = await import('./useAlertStream');
    renderHook(() => useAlertStream());
    await waitFor(() => expect(subscribeAlerts).toHaveBeenCalled());
    upsertAlert.mockClear();

    onUpsert(alert('live-1'));
    expect(upsertAlert).toHaveBeenCalledWith(alert('live-1'));

    onResolve(alert('live-1'));
    expect(resolveAlert).toHaveBeenCalledWith(alert('live-1'));
  });

  it('closes the stream on unmount', async () => {
    const { useAlertStream } = await import('./useAlertStream');
    const { unmount } = renderHook(() => useAlertStream());
    await waitFor(() => expect(subscribeAlerts).toHaveBeenCalled());

    unmount();
    expect(unsubscribe).toHaveBeenCalledTimes(1);
  });
});
