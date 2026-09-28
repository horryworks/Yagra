// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { inputFromJob } from './format';
import { BASELINE_SECS, DEFAULT_WINDOW_SECS } from './analysisDefaults';
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
    expect(inp.window_secs).toBe(DEFAULT_WINDOW_SECS);
    expect(inp.baseline_secs).toBe(BASELINE_SECS);
    expect(inp.sensitivity).toBe(3.0);
    expect(inp.depth).toBe('standard');
    expect(inp.family).toBe('all');
    // A non-boolean `notify` (here: absent) defaults to true.
    expect(inp.notify).toBe(true);
  });

  it('re-runs a job that recorded no window over the seven-day default, like every launcher', () => {
    // The drift ADR-184 increment 6 closed: this fallback said 24 hours while the drawer, the
    // schedule form and the quick run all said 7 days, so "Re-run" on an old row silently ran a
    // shorter analysis than the one it copied. Pinned as a number, not as the constant, so the
    // test is about the behaviour an operator sees and not about which name the code reads.
    const inp = inputFromJob(job({ params: {} }));
    expect(inp.window_secs).toBe(7 * 24 * 3600);
  });
});
