// Troubleshoot UI state (Zustand). Ephemeral live data — the async-jobs list, the launch
// drawer, and the transient toast — so it is NOT persisted (coding-conventions: live data ⇒
// component/hook state, not the persisted store). Today the runs are seeded from mock data
// (data.ts); once a jobs API exists this store is the seam that swaps to it (poll/SSE feed
// `runs`, `addRun`/`cancelRun`/`retryRun` call the API).

import { create } from 'zustand';
import { INITIAL_RUNS, type Run } from './data';

export interface Toast {
  msg: string;
  /** Optional in-app route the "View →" action navigates to. */
  linkTo?: string;
  /** Bumps on every showToast so the auto-dismiss timer restarts even for the same message. */
  key: number;
}

interface TroubleshootStore {
  runs: Run[];
  /** Monotonic counter for generated run ids (deterministic, no Math.random for keys). */
  nextRunId: number;

  /** Prepend a freshly-launched run (drawer submit). */
  addRun: (run: Omit<Run, 'id'>) => void;
  /** Cancel a running job — drops it from the list. */
  cancelRun: (id: string) => void;
  /** Re-queue a failed job back to running. */
  retryRun: (id: string) => void;
  /** Advance every running job's progress one tick (drives the live feel). */
  tickProgress: () => void;

  /** Currently-open tool in the launch drawer (id), or null when closed. */
  openToolId: string | null;
  openDrawer: (toolId: string) => void;
  closeDrawer: () => void;

  toast: Toast | null;
  showToast: (msg: string, linkTo?: string) => void;
  dismissToast: () => void;
}

export const useTroubleshootStore = create<TroubleshootStore>((set) => ({
  runs: INITIAL_RUNS,
  nextRunId: 1,

  addRun: (run) =>
    set((s) => ({
      runs: [{ ...run, id: `new-${s.nextRunId}` }, ...s.runs],
      nextRunId: s.nextRunId + 1,
    })),

  cancelRun: (id) => set((s) => ({ runs: s.runs.filter((r) => r.id !== id) })),

  retryRun: (id) =>
    set((s) => ({
      runs: s.runs.map((r) =>
        r.id === id && r.state === 'failed'
          ? {
              ...r,
              state: 'running',
              pct: 3,
              phase: 'Queued — fetching history…',
              eta: '~2m',
              started: 'just now',
              err: undefined,
              when: undefined,
            }
          : r,
      ),
    })),

  tickProgress: () =>
    set((s) => {
      if (!s.runs.some((r) => r.state === 'running')) return s;
      return {
        runs: s.runs.map((r) => {
          if (r.state !== 'running') return r;
          // Climb toward — but never reach — 100%; real completion comes from job status.
          const pct = Math.min(99, (r.pct ?? 0) + Math.random() * 5);
          return { ...r, pct, phase: pct > 92 ? 'Finalizing report…' : r.phase };
        }),
      };
    }),

  openToolId: null,
  openDrawer: (toolId) => set({ openToolId: toolId }),
  closeDrawer: () => set({ openToolId: null }),

  toast: null,
  showToast: (msg, linkTo) =>
    set((s) => ({ toast: { msg, linkTo, key: (s.toast?.key ?? 0) + 1 } })),
  dismissToast: () => set({ toast: null }),
}));

/** Number of jobs currently running (drives the sidebar "Analysis runs" badge + intro stat). */
export function runningCount(runs: Run[]): number {
  return runs.filter((r) => r.state === 'running').length;
}
