import { beforeEach, describe, expect, it } from 'vitest';
import { INITIAL_RUNS } from './data';
import { runningCount, useTroubleshootStore } from './store';

function reset() {
  useTroubleshootStore.setState({
    runs: INITIAL_RUNS.map((r) => ({ ...r })),
    nextRunId: 1,
    openToolId: null,
    toast: null,
  });
}

beforeEach(reset);

describe('troubleshoot runs store', () => {
  it('seeds with two running jobs', () => {
    expect(runningCount(useTroubleshootStore.getState().runs)).toBe(2);
  });

  it('prepends a launched run with a generated id', () => {
    useTroubleshootStore.getState().addRun({
      tool: 'Anomaly Detection',
      mono: 'An',
      scope: 'all nodes (128) · 7 d',
      state: 'running',
      pct: 3,
    });
    const runs = useTroubleshootStore.getState().runs;
    expect(runs).toHaveLength(INITIAL_RUNS.length + 1);
    expect(runs[0].tool).toBe('Anomaly Detection');
    expect(runs[0].id).toBe('new-1');
    expect(runningCount(runs)).toBe(3);
  });

  it('cancel drops a running job from the list', () => {
    useTroubleshootStore.getState().cancelRun('r1');
    const runs = useTroubleshootStore.getState().runs;
    expect(runs.find((r) => r.id === 'r1')).toBeUndefined();
    expect(runningCount(runs)).toBe(1);
  });

  it('retry re-queues a failed job back to running', () => {
    expect(useTroubleshootStore.getState().runs.find((r) => r.id === 'r5')?.state).toBe('failed');
    useTroubleshootStore.getState().retryRun('r5');
    const r5 = useTroubleshootStore.getState().runs.find((r) => r.id === 'r5');
    expect(r5?.state).toBe('running');
    expect(r5?.err).toBeUndefined();
    expect(runningCount(useTroubleshootStore.getState().runs)).toBe(3);
  });

  it('retry is a no-op on a non-failed job', () => {
    useTroubleshootStore.getState().retryRun('r3'); // a done job
    expect(useTroubleshootStore.getState().runs.find((r) => r.id === 'r3')?.state).toBe('done');
  });

  it('tick advances running progress, caps at 99, and never touches finished jobs', () => {
    useTroubleshootStore.setState({
      runs: [
        { id: 'a', tool: 'T', mono: 'T', scope: 's', state: 'running', pct: 95, phase: 'Scoring…' },
        { id: 'b', tool: 'T', mono: 'T', scope: 's', state: 'done', when: '1m ago' },
      ],
      nextRunId: 1,
      openToolId: null,
      toast: null,
    });
    useTroubleshootStore.getState().tickProgress();
    const [a, b] = useTroubleshootStore.getState().runs;
    expect(a.pct).toBeGreaterThanOrEqual(95);
    expect(a.pct).toBeLessThanOrEqual(99);
    expect(a.phase).toBe('Finalizing report…'); // >92%
    expect(b.state).toBe('done'); // untouched
  });
});

describe('troubleshoot drawer + toast', () => {
  it('opens and closes the launch drawer by tool id', () => {
    useTroubleshootStore.getState().openDrawer('capacity');
    expect(useTroubleshootStore.getState().openToolId).toBe('capacity');
    useTroubleshootStore.getState().closeDrawer();
    expect(useTroubleshootStore.getState().openToolId).toBeNull();
  });

  it('shows a toast and bumps its key so a repeat message re-fires the timer', () => {
    useTroubleshootStore.getState().showToast('Started', '/troubleshoot/anomaly');
    const first = useTroubleshootStore.getState().toast;
    expect(first?.msg).toBe('Started');
    expect(first?.linkTo).toBe('/troubleshoot/anomaly');
    useTroubleshootStore.getState().showToast('Started');
    expect(useTroubleshootStore.getState().toast?.key).toBe((first?.key ?? 0) + 1);
    useTroubleshootStore.getState().dismissToast();
    expect(useTroubleshootStore.getState().toast).toBeNull();
  });
});
