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
const subscribeAnalysis = vi.fn((cb: (j: AnalysisJob) => void) => {
  onJob = cb;
  return unsubscribe;
});

const setJobs = vi.fn();
const upsertJob = vi.fn();

vi.mock('../services/api', () => ({
  api: { listAnalysisJobs: (n: number) => listAnalysisJobs(n) },
}));
vi.mock('../services/sse', () => ({
  subscribeAnalysis: (cb: (j: AnalysisJob) => void) => subscribeAnalysis(cb),
}));
vi.mock('./store', () => ({
  useTroubleshootStore: (sel: (s: unknown) => unknown) => sel({ setJobs, upsertJob }),
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

  it('feeds live job updates into the store', async () => {
    const { useTroubleshootStream } = await import('./useTroubleshootStream');
    renderHook(() => useTroubleshootStream());
    await waitFor(() => expect(subscribeAnalysis).toHaveBeenCalled());

    onJob(job('j2'));
    expect(upsertJob).toHaveBeenCalledWith(job('j2'));
  });

  it('closes the stream on unmount', async () => {
    const { useTroubleshootStream } = await import('./useTroubleshootStream');
    const { unmount } = renderHook(() => useTroubleshootStream());
    await waitFor(() => expect(subscribeAnalysis).toHaveBeenCalled());

    unmount();
    expect(unsubscribe).toHaveBeenCalledTimes(1);
  });
});
