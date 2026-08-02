// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { isFinished, reportLinkFor, shouldNotify } from './notifyWatch';
import type { AnalysisJob } from '../types/api';

const job = (state: string, id = 'j1', tool = 'anomaly'): AnalysisJob =>
  ({ id, tool, state, created_ms: 1 }) as AnalysisJob;

const watching = new Set(['j1']);

describe('isFinished', () => {
  it('treats anything not in flight as finished', () => {
    expect(isFinished('running')).toBe(false);
    expect(isFinished('queued')).toBe(false);
    expect(isFinished('succeeded')).toBe(true);
    expect(isFinished('failed')).toBe(true);
    expect(isFinished('cancelled')).toBe(true);
  });

  it('treats a state this build does not know as finished', () => {
    // Better to notify once too often than to leave the operator waiting on a run that a newer
    // core has already ended under a name this build has never heard of.
    expect(isFinished('exploded')).toBe(true);
  });
});

describe('shouldNotify', () => {
  it('fires once, on the transition into a finished state', () => {
    const plan = shouldNotify(job('running'), job('succeeded'), watching);
    expect(plan?.msgKey).toBe('toast.finished');
  });

  it('does not fire again for a repeated terminal tick', () => {
    // SSE can redeliver the same terminal row; the operator must not get two toasts.
    expect(shouldNotify(job('succeeded'), job('succeeded'), watching)).toBeNull();
  });

  it('does not fire while the job is still running', () => {
    expect(shouldNotify(job('queued'), job('running'), watching)).toBeNull();
  });

  it('does not fire for a job the operator did not ask to be notified about', () => {
    expect(shouldNotify(job('running'), job('succeeded'), new Set())).toBeNull();
  });

  it('does not fire on first sighting of an already-finished job', () => {
    // A late subscribe or a reload after the fact: announcing it would be noise, not news.
    expect(shouldNotify(undefined, job('succeeded'), watching)).toBeNull();
  });

  it('distinguishes a failure from a success', () => {
    expect(shouldNotify(job('running'), job('failed'), watching)?.msgKey).toBe('toast.failed');
    expect(shouldNotify(job('running'), job('cancelled'), watching)?.msgKey).toBe('toast.failed');
  });
});

describe('reportLinkFor', () => {
  it('links to the tool report carrying the job id', () => {
    expect(reportLinkFor(job('succeeded'))).toBe('/troubleshoot/report/anomaly?job=j1');
  });

  it('falls back to the catalog for a tool this build does not have', () => {
    expect(reportLinkFor(job('succeeded', 'j1', 'from-the-future'))).toBe('/troubleshoot');
  });
});
