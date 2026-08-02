// SPDX-License-Identifier: AGPL-3.0-only
// Troubleshoot UI state (Zustand). The async-jobs list is now real — seeded from the jobs API
// and kept live over SSE (ADR-022) — so this store holds the job list plus the ephemeral launch
// drawer and toast. Live data ⇒ not persisted (coding-conventions).

import { create } from 'zustand';
import { api } from '../services/api';
import type { AnalysisJob, AnalysisJobInput } from '../types/api';
import { shouldNotify } from './notifyWatch';

export interface Toast {
  /** Already-translated text. Empty when [`msgKey`] carries the message instead. */
  msg: string;
  /** An i18next key under `troubleshoot:`, translated by the toast component. Used when the
   *  message originates in the store, which has no `t`. */
  msgKey?: string;
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

  /** Jobs the operator asked to be notified about (launch drawer, Notify me). Session-scoped:
   *  the notice is an in-app toast, so it has no meaning once this tab is gone. */
  watched: Set<string>;
  /** Start watching a job for completion. */
  watchJob: (id: string) => void;
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

  watched: new Set<string>(),
  watchJob: (id) => set((s) => ({ watched: new Set(s.watched).add(id) })),

  upsertJob: (job) =>
    set((s) => {
      const prev = s.jobs.find((j) => j.id === job.id);
      const rest = s.jobs.filter((j) => j.id !== job.id);
      const next: Partial<TroubleshootStore> = { jobs: [job, ...rest].sort(byNewest) };
      // Completion notice for a watched job. Computed here rather than in the stream hook so it
      // fires for every route into the store (SSE tick, cancel, create) and exactly once — the
      // previous row is only available at this point.
      const plan = shouldNotify(prev, job, s.watched);
      if (plan) {
        next.toast = { msg: '', msgKey: plan.msgKey, linkTo: plan.linkTo, key: (s.toast?.key ?? 0) + 1 };
        next.watched = new Set([...s.watched].filter((id) => id !== job.id));
      }
      return next;
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
