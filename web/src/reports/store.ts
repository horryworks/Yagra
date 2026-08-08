// SPDX-License-Identifier: AGPL-3.0-only
// Report-runs state (Zustand). The saved-reports list is seeded from the runs API and kept live
// over SSE (generation progress) — live data, so not persisted (coding-conventions). Mirrors the
// troubleshoot runs store.

import { create } from 'zustand';
import type { ReportRun } from '../types/api';

interface ReportRunsStore {
  runs: ReportRun[];
  /** True once the initial fetch has resolved (so the UI can tell "empty" from "loading"). */
  loaded: boolean;
  setRuns: (runs: ReportRun[]) => void;
  /** Upsert one run by id (SSE tick or run-now response), newest-first. */
  upsertRun: (run: ReportRun) => void;
  /** Drop a run (after delete). */
  removeRun: (id: string) => void;
}

function byNewest(a: ReportRun, b: ReportRun): number {
  return b.created_ms - a.created_ms;
}

export const useReportRunsStore = create<ReportRunsStore>((set) => ({
  runs: [],
  loaded: false,
  setRuns: (runs) => set({ runs: [...runs].sort(byNewest), loaded: true }),
  upsertRun: (run) =>
    set((s) => {
      const rest = s.runs.filter((r) => r.id !== run.id);
      return { runs: [run, ...rest].sort(byNewest) };
    }),
  removeRun: (id) => set((s) => ({ runs: s.runs.filter((r) => r.id !== id) })),
}));

