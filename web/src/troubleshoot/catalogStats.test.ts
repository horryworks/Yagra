// SPDX-License-Identifier: AGPL-3.0-only
// The Troubleshoot catalog header's two numbers. Both read from a job list whose timestamps are
// nullable and, for a cancelled job, can run backwards.

import { describe, expect, it } from 'vitest';
import { avgRuntime, runsToday } from './catalogStats';
import type { AnalysisJob } from '../types/api';

const job = (over: Partial<AnalysisJob> = {}): AnalysisJob =>
  ({ id: 'j', created_ms: 0, started_ms: null, finished_ms: null, ...over }) as AnalysisJob;

const NOW = 1_700_000_000_000;
const DAY = 86_400_000;

describe('runsToday', () => {
  it('counts only the last 24 hours, inclusive at the boundary', () => {
    const jobs = [
      job({ created_ms: NOW }),
      job({ created_ms: NOW - DAY }), // exactly 24h old — still counted
      job({ created_ms: NOW - DAY - 1 }), // one ms older — not
    ];
    expect(runsToday(jobs, NOW)).toBe(2);
  });

  it('is zero for an empty catalog', () => {
    expect(runsToday([], NOW)).toBe(0);
  });
});

describe('avgRuntime', () => {
  it('shows a dash when nothing has finished', () => {
    expect(avgRuntime([])).toBe('—');
    expect(avgRuntime([job({ started_ms: NOW })])).toBe('—');
  });

  it('ignores a job missing either timestamp', () => {
    // A running job has no `finished_ms`; averaging it in as zero would understate the mean.
    const jobs = [
      job({ started_ms: NOW, finished_ms: NOW + 10_000 }),
      job({ started_ms: NOW, finished_ms: null }),
      job({ started_ms: null, finished_ms: NOW }),
    ];
    expect(avgRuntime(jobs)).toBe('10s');
  });

  it('discards a negative span rather than dragging the mean down', () => {
    // A job cancelled before it started reports finished < started.
    const jobs = [
      job({ started_ms: NOW, finished_ms: NOW + 20_000 }),
      job({ started_ms: NOW + 5_000, finished_ms: NOW }),
    ];
    expect(avgRuntime(jobs)).toBe('20s');
  });

  it('formats past a minute as "Nm Ns"', () => {
    expect(avgRuntime([job({ started_ms: 0, finished_ms: 90_000 })])).toBe('1m 30s');
    expect(avgRuntime([job({ started_ms: 0, finished_ms: 60_000 })])).toBe('1m 0s');
    expect(avgRuntime([job({ started_ms: 0, finished_ms: 59_000 })])).toBe('59s');
  });

  it('averages several finished jobs', () => {
    const jobs = [
      job({ started_ms: 0, finished_ms: 10_000 }),
      job({ started_ms: 0, finished_ms: 30_000 }),
    ];
    expect(avgRuntime(jobs)).toBe('20s');
  });
});
