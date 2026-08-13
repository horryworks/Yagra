// SPDX-License-Identifier: AGPL-3.0-only
//
// ⚠️ Every case here used to say `'succeeded'`, which is a *report* run's word for finished — an
// analysis run says `done`. `AnalysisJob.state` was a bare `string`, so the fixtures compiled, the
// production comparison against `'succeeded'` compiled, and both were wrong in the same direction:
// the test agreed with the bug. "distinguishes a failure from a success" passed while only ever
// exercising the failure branch, because its success case was a state that cannot occur.
//
// The fixture is typed now, so the vocabulary is the compiler's problem rather than this comment's.

import { describe, expect, it } from 'vitest';
import { isFinished, reportLinkFor, shouldNotify } from './notifyWatch';
import type { AnalysisJob, AnalysisJobState } from '../types/api';

const job = (state: AnalysisJobState, id = 'j1', tool = 'anomaly'): AnalysisJob =>
  ({ id, tool, state, created_ms: 1 }) as AnalysisJob;

const watching = new Set(['j1']);

describe('isFinished', () => {
  it('treats anything not in flight as finished', () => {
    expect(isFinished('running')).toBe(false);
    expect(isFinished('queued')).toBe(false);
    expect(isFinished('done')).toBe(true);
    expect(isFinished('failed')).toBe(true);
    expect(isFinished('cancelled')).toBe(true);
  });

  it('treats a state this build does not know as finished', () => {
    // Better to notify once too often than to leave the operator waiting on a run that a newer
    // core has already ended under a name this build has never heard of. `unknown` is what such a
    // token degrades to — the backend does the same, for the same reason.
    expect(isFinished('unknown')).toBe(true);
  });
});

describe('shouldNotify', () => {
  it('fires once, on the transition into a finished state', () => {
    const plan = shouldNotify(job('running'), job('done'), watching);
    expect(plan?.msgKey).toBe('toast.finished');
  });

  it('does not fire again for a repeated terminal tick', () => {
    // SSE can redeliver the same terminal row; the operator must not get two toasts.
    expect(shouldNotify(job('done'), job('done'), watching)).toBeNull();
  });

  it('does not fire while the job is still running', () => {
    expect(shouldNotify(job('queued'), job('running'), watching)).toBeNull();
  });

  it('does not fire for a job the operator did not ask to be notified about', () => {
    expect(shouldNotify(job('running'), job('done'), new Set())).toBeNull();
  });

  it('does not fire on first sighting of an already-finished job', () => {
    // A late subscribe or a reload after the fact: announcing it would be noise, not news.
    expect(shouldNotify(undefined, job('done'), watching)).toBeNull();
  });

  it('distinguishes a failure from a success', () => {
    // The assertion that was never really made. A run that finished normally must say so — this
    // is the case that was announcing "your analysis failed" to every operator who used Notify me.
    expect(shouldNotify(job('running'), job('done'), watching)?.msgKey).toBe('toast.finished');
    expect(shouldNotify(job('running'), job('failed'), watching)?.msgKey).toBe('toast.failed');
    expect(shouldNotify(job('running'), job('cancelled'), watching)?.msgKey).toBe('toast.failed');
    // A state a newer core invented is not a success, so it must not be announced as one.
    expect(shouldNotify(job('running'), job('unknown'), watching)?.msgKey).toBe('toast.failed');
  });
});

describe('reportLinkFor', () => {
  it('links to the tool report carrying the job id', () => {
    expect(reportLinkFor(job('done'))).toBe('/troubleshoot/report/anomaly?job=j1');
  });

  it('falls back to the catalog for a tool this build does not have', () => {
    expect(reportLinkFor(job('done', 'j1', 'from-the-future'))).toBe('/troubleshoot');
  });
});
