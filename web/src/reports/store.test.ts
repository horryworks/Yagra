import { beforeEach, describe, expect, it } from 'vitest';
import { generatingCount, useReportRunsStore } from './store';
import type { ReportRun } from '../types/api';

function run(over: Partial<ReportRun>): ReportRun {
  return {
    id: 'r1',
    definition_id: null,
    name: 'Weekly rollup',
    trigger: 'manual',
    state: 'succeeded',
    pct: 100,
    error: null,
    range_from_ms: null,
    range_to_ms: null,
    section_count: 0,
    created_by: null,
    created_ms: 1000,
    started_ms: 1000,
    finished_ms: 2000,
    ...over,
  };
}

beforeEach(() => {
  useReportRunsStore.setState({ runs: [], loaded: false });
});

describe('report runs store', () => {
  it('setRuns sorts newest-first and marks loaded', () => {
    useReportRunsStore.getState().setRuns([
      run({ id: 'a', created_ms: 100 }),
      run({ id: 'b', created_ms: 300 }),
      run({ id: 'c', created_ms: 200 }),
    ]);
    const s = useReportRunsStore.getState();
    expect(s.loaded).toBe(true);
    expect(s.runs.map((r) => r.id)).toEqual(['b', 'c', 'a']);
  });

  it('setRuns sorts a copy — the caller array is left untouched', () => {
    const input = [run({ id: 'a', created_ms: 100 }), run({ id: 'b', created_ms: 300 })];
    useReportRunsStore.getState().setRuns(input);
    expect(input.map((r) => r.id)).toEqual(['a', 'b']);
  });

  it('upsertRun inserts a new run keeping newest-first order', () => {
    const store = useReportRunsStore.getState();
    store.setRuns([run({ id: 'a', created_ms: 100 })]);
    store.upsertRun(run({ id: 'b', created_ms: 300 }));
    expect(useReportRunsStore.getState().runs.map((r) => r.id)).toEqual(['b', 'a']);
  });

  it('upsertRun replaces an existing run by id (SSE tick) without duplicating', () => {
    const store = useReportRunsStore.getState();
    store.setRuns([run({ id: 'a', created_ms: 100, pct: 10, state: 'running' })]);
    store.upsertRun(run({ id: 'a', created_ms: 100, pct: 90, state: 'running' }));
    const runs = useReportRunsStore.getState().runs;
    expect(runs).toHaveLength(1);
    expect(runs[0].pct).toBe(90);
  });

  it('removeRun drops a run by id', () => {
    const store = useReportRunsStore.getState();
    store.setRuns([run({ id: 'a' }), run({ id: 'b' })]);
    store.removeRun('a');
    expect(useReportRunsStore.getState().runs.map((r) => r.id)).toEqual(['b']);
  });
});

describe('generatingCount', () => {
  it('counts only running or queued runs', () => {
    const runs = [
      run({ id: 'a', state: 'running' }),
      run({ id: 'b', state: 'queued' }),
      run({ id: 'c', state: 'succeeded' }),
      run({ id: 'd', state: 'failed' }),
    ];
    expect(generatingCount(runs)).toBe(2);
  });

  it('is zero when nothing is in flight', () => {
    expect(generatingCount([run({ state: 'succeeded' }), run({ state: 'failed' })])).toBe(0);
  });
});
