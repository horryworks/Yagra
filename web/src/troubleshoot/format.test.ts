// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { inputFromJob, relTime } from './format';
import type { AnalysisJob } from '../types/api';

function job(over: Partial<AnalysisJob>): AnalysisJob {
  return {
    id: 'j1',
    tool: 'anomaly',
    scope_kind: 'all',
    scope_id: null,
    scope_label: 'All nodes',
    params: {},
    state: 'done',
    pct: 100,
    phase: null,
    finding_count: 0,
    summary: null,
    error: null,
    created_ms: 1000,
    started_ms: 1000,
    finished_ms: 2000,
    ...over,
  };
}

describe('inputFromJob', () => {
  it('rebuilds the launch request from a job whose params are all present', () => {
    const inp = inputFromJob(
      job({
        tool: 'capacity',
        scope_kind: 'group',
        scope_id: 'g1',
        scope_label: 'Core',
        params: {
          window_secs: 3600,
          baseline_secs: 86_400,
          sensitivity: 2.5,
          depth: 'deep',
          family: 'cpu',
          notify: false,
        },
      }),
    );
    expect(inp).toEqual({
      tool: 'capacity',
      scope_kind: 'group',
      scope_id: 'g1',
      scope_label: 'Core',
      window_secs: 3600,
      baseline_secs: 86_400,
      sensitivity: 2.5,
      depth: 'deep',
      family: 'cpu',
      notify: false,
    });
  });

  it('falls back to defaults when params are missing or the wrong type', () => {
    const inp = inputFromJob(job({ params: { window_secs: 'oops', sensitivity: null } }));
    expect(inp.window_secs).toBe(24 * 3600);
    expect(inp.baseline_secs).toBe(14 * 86_400);
    expect(inp.sensitivity).toBe(3.0);
    expect(inp.depth).toBe('standard');
    expect(inp.family).toBe('all');
    // A non-boolean `notify` (here: absent) defaults to true.
    expect(inp.notify).toBe(true);
  });
});

describe('relTime', () => {
  it('is empty for a missing timestamp', () => {
    expect(relTime(null)).toBe('');
    expect(relTime(undefined)).toBe('');
    expect(relTime(0)).toBe('');
  });

  it('buckets a recent timestamp into just-now / minutes / hours / days', () => {
    const now = Date.now();
    expect(relTime(now - 5_000)).toBe('just now');
    expect(relTime(now - 18 * 60_000)).toBe('18m ago');
    expect(relTime(now - 2 * 3_600_000)).toBe('2h ago');
    expect(relTime(now - 3 * 86_400_000)).toBe('3d ago');
  });
});
