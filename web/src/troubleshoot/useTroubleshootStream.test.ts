// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { AnalysisJob } from '../types/api';

// The analysis analog of useAlertStream (ADR-022): seed the runs list once, then stay live. Same
// contract — a failed seed must not cost the subscription, and unmount must close it.

const listAnalysisJobs = vi.fn();
const unsubscribe = vi.fn();
let onJob: (j: AnalysisJob) => void = () => {};
let onResync: (() => void) | undefined;
const subscribeAnalysis = vi.fn(
  (cb: (j: AnalysisJob) => void, _err?: unknown, resync?: () => void) => {
    onJob = cb;
    onResync = resync;
    return unsubscribe;
  },
);

const setJobs = vi.fn();
const setLoadFailed = vi.fn();
const upsertJob = vi.fn();

vi.mock('../services/api', () => ({
  api: { listAnalysisJobs: (n: number) => listAnalysisJobs(n) },
}));
vi.mock('../services/sse', () => ({
  subscribeAnalysis: (cb: (j: AnalysisJob) => void, err?: unknown, resync?: () => void) =>
    subscribeAnalysis(cb, err, resync),
}));
vi.mock('./store', () => ({
  // The seed reads the store outside React (`getState`) so the runs list can call it for a retry.
  useTroubleshootStore: Object.assign(
    (sel: (s: unknown) => unknown) => sel({ setJobs, setLoadFailed, upsertJob }),
    { getState: () => ({ setJobs, setLoadFailed, upsertJob }) },
  ),
}));

const job = (id: string): AnalysisJob => ({ id, state: 'running' }) as unknown as AnalysisJob;

describe('useTroubleshootStream', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    listAnalysisJobs.mockResolvedValue([job('j1')]);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('seeds the recent-jobs list and opens the stream', async () => {
    const { useTroubleshootStream } = await import('./useTroubleshootStream');
    renderHook(() => useTroubleshootStream());

    await waitFor(() => expect(setJobs).toHaveBeenCalledWith([job('j1')]));
    expect(listAnalysisJobs).toHaveBeenCalledWith(50);
    expect(subscribeAnalysis).toHaveBeenCalledTimes(1);
  });

  it('still subscribes when the seed fetch fails', async () => {
    listAnalysisJobs.mockRejectedValue(new Error('gated'));
    const { useTroubleshootStream } = await import('./useTroubleshootStream');
    renderHook(() => useTroubleshootStream());

    await waitFor(() => expect(subscribeAnalysis).toHaveBeenCalledTimes(1));
    expect(setJobs).not.toHaveBeenCalled();
  });

  it('records a failed seed, so the runs list can say so instead of loading for ever', async () => {
    // `loaded` is set by `setJobs` alone. With the failure swallowed, `/troubleshoot/runs` read
    // "Loading…" for as long as the app stayed open.
    listAnalysisJobs.mockRejectedValue(new Error('503'));
    const { useTroubleshootStream } = await import('./useTroubleshootStream');
    renderHook(() => useTroubleshootStream());

    await waitFor(() => expect(setLoadFailed).toHaveBeenCalledWith(true));
    expect(setJobs).not.toHaveBeenCalled();
  });

  it('a retry clears the failure before it asks again', async () => {
    const { seedAnalysisJobs } = await import('./useTroubleshootStream');
    seedAnalysisJobs();
    expect(setLoadFailed).toHaveBeenCalledWith(false);
    await waitFor(() => expect(setJobs).toHaveBeenCalledWith([job('j1')]));
    expect(setLoadFailed).not.toHaveBeenCalledWith(true);
  });

  it('feeds live job updates into the store', async () => {
    const { useTroubleshootStream } = await import('./useTroubleshootStream');
    renderHook(() => useTroubleshootStream());
    await waitFor(() => expect(subscribeAnalysis).toHaveBeenCalled());

    onJob(job('j2'));
    expect(upsertJob).toHaveBeenCalledWith(job('j2'));
  });

  it('re-reads the recent-jobs list on a resync (a reconnect replays nothing)', async () => {
    const { useTroubleshootStream } = await import('./useTroubleshootStream');
    renderHook(() => useTroubleshootStream());
    await waitFor(() => expect(setJobs).toHaveBeenCalledTimes(1));
    expect(onResync).toBeTypeOf('function');

    listAnalysisJobs.mockResolvedValue([job('j1'), job('j3')]);
    onResync!();
    await waitFor(() => expect(setJobs).toHaveBeenCalledTimes(2));
    expect(setJobs).toHaveBeenLastCalledWith([job('j1'), job('j3')]);
  });

  it('closes the stream on unmount', async () => {
    const { useTroubleshootStream } = await import('./useTroubleshootStream');
    const { unmount } = renderHook(() => useTroubleshootStream());
    await waitFor(() => expect(subscribeAnalysis).toHaveBeenCalled());

    unmount();
    expect(unsubscribe).toHaveBeenCalledTimes(1);
  });
});
