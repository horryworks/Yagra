import { beforeEach, describe, expect, it } from 'vitest';
import { runningCount, useTroubleshootStore } from './store';
import type { AnalysisJob } from '../types/api';

function job(over: Partial<AnalysisJob>): AnalysisJob {
  return {
    id: 'j1',
    tool: 'anomaly',
    scope_kind: 'all',
    scope_id: null,
    scope_label: 'All nodes',
    params: {},
    state: 'running',
    pct: 10,
    phase: 'Scoring…',
    finding_count: 0,
    summary: null,
    error: null,
    created_ms: 1000,
    started_ms: 1000,
    finished_ms: null,
    ...over,
  };
}

beforeEach(() => {
  useTroubleshootStore.setState({ jobs: [], loaded: false, openToolId: null, toast: null });
});

describe('troubleshoot jobs store', () => {
  it('setJobs sorts newest-first and marks loaded', () => {
    useTroubleshootStore.getState().setJobs([
      job({ id: 'a', created_ms: 100 }),
      job({ id: 'b', created_ms: 300 }),
      job({ id: 'c', created_ms: 200 }),
    ]);
    const s = useTroubleshootStore.getState();
    expect(s.loaded).toBe(true);
    expect(s.jobs.map((j) => j.id)).toEqual(['b', 'c', 'a']);
  });

  it('upsertJob replaces by id (SSE tick) and keeps newest-first order', () => {
    const store = useTroubleshootStore.getState();
    store.setJobs([job({ id: 'a', created_ms: 100, pct: 10 })]);
    store.upsertJob(job({ id: 'b', created_ms: 300, pct: 5 }));
    store.upsertJob(job({ id: 'a', created_ms: 100, pct: 90 })); // update existing
    const jobs = useTroubleshootStore.getState().jobs;
    expect(jobs.map((j) => j.id)).toEqual(['b', 'a']);
    expect(jobs.find((j) => j.id === 'a')?.pct).toBe(90);
  });

  it('runningCount counts only running jobs', () => {
    const jobs = [
      job({ id: 'a', state: 'running' }),
      job({ id: 'b', state: 'done' }),
      job({ id: 'c', state: 'running' }),
      job({ id: 'd', state: 'failed' }),
    ];
    expect(runningCount(jobs)).toBe(2);
  });

  it('opens and closes the launch drawer by tool id', () => {
    useTroubleshootStore.getState().openDrawer('capacity');
    expect(useTroubleshootStore.getState().openToolId).toBe('capacity');
    useTroubleshootStore.getState().closeDrawer();
    expect(useTroubleshootStore.getState().openToolId).toBeNull();
  });

  it('shows a toast and bumps its key for a repeat message', () => {
    useTroubleshootStore.getState().showToast('Started', '/troubleshoot/anomaly');
    const first = useTroubleshootStore.getState().toast;
    expect(first?.msg).toBe('Started');
    expect(first?.linkTo).toBe('/troubleshoot/anomaly');
    useTroubleshootStore.getState().showToast('Started again');
    expect(useTroubleshootStore.getState().toast?.key).toBe((first?.key ?? 0) + 1);
    useTroubleshootStore.getState().dismissToast();
    expect(useTroubleshootStore.getState().toast).toBeNull();
  });
});
