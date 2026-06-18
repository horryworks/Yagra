// Troubleshoot UI state (Zustand). The async-jobs list is now real — seeded from the jobs API
// and kept live over SSE (ADR-022) — so this store holds the job list plus the ephemeral launch
// drawer and toast. Live data ⇒ not persisted (coding-conventions).

import { create } from 'zustand';
import { api } from '../services/api';
import type { AnalysisJob, AnalysisJobInput } from '../types/api';

export interface Toast {
  msg: string;
  /** Optional in-app route the "View →" action navigates to. */
  linkTo?: string;
  /** Bumps on every showToast so the auto-dismiss timer restarts for a repeat message. */
  key: number;
}

interface TroubleshootStore {
  jobs: AnalysisJob[];
  /** True once the initial fetch has resolved (so the UI can tell "empty" from "loading"). */
  loaded: boolean;

  setJobs: (jobs: AnalysisJob[]) => void;
  /** Upsert one job by id (SSE tick or create response), keeping newest-first order. */
  upsertJob: (job: AnalysisJob) => void;
  /** Launch a job; returns the created row (already upserted). */
  createJob: (input: AnalysisJobInput) => Promise<AnalysisJob>;
  /** Cancel a running job (optimistically marks it cancelled; SSE confirms). */
  cancelJob: (id: string) => Promise<void>;

  /** Currently-open tool in the launch drawer (id), or null when closed. */
  openToolId: string | null;
  openDrawer: (toolId: string) => void;
  closeDrawer: () => void;

  toast: Toast | null;
  showToast: (msg: string, linkTo?: string) => void;
  dismissToast: () => void;
}

function byNewest(a: AnalysisJob, b: AnalysisJob): number {
  return b.created_ms - a.created_ms;
}

export const useTroubleshootStore = create<TroubleshootStore>((set, get) => ({
  jobs: [],
  loaded: false,

  setJobs: (jobs) => set({ jobs: [...jobs].sort(byNewest), loaded: true }),

  upsertJob: (job) =>
    set((s) => {
      const rest = s.jobs.filter((j) => j.id !== job.id);
      return { jobs: [job, ...rest].sort(byNewest) };
    }),

  createJob: async (input) => {
    const job = await api.createAnalysisJob(input);
    get().upsertJob(job);
    return job;
  },

  cancelJob: async (id) => {
    await api.cancelAnalysisJob(id);
    // Optimistic terminal state; the SSE stream will deliver the authoritative row.
    set((s) => ({
      jobs: s.jobs.map((j) =>
        j.id === id && j.state === 'running'
          ? { ...j, state: 'cancelled', phase: null }
          : j,
      ),
    }));
  },

  openToolId: null,
  openDrawer: (toolId) => set({ openToolId: toolId }),
  closeDrawer: () => set({ openToolId: null }),

  toast: null,
  showToast: (msg, linkTo) =>
    set((s) => ({ toast: { msg, linkTo, key: (s.toast?.key ?? 0) + 1 } })),
  dismissToast: () => set({ toast: null }),
}));

/** Number of jobs currently running (drives the sidebar badge + intro stat). */
export function runningCount(jobs: AnalysisJob[]): number {
  return jobs.filter((j) => j.state === 'running').length;
}
