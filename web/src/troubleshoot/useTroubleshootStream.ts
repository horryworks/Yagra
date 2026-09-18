// SPDX-License-Identifier: AGPL-3.0-only
// Seeds the analysis-runs list once, then keeps it live over SSE (ADR-022) — the analysis analog
// of useAlertStream. Mounted once by AppShell — not per page — so a run launched with "notify me"
// still reports its completion after the operator navigates away. The sidebar badge reads the store.

import { useEffect } from 'react';
import { api } from '../services/api';
import { subscribeAnalysis } from '../services/sse';
import { useTroubleshootStore } from './store';

/** Fetch the recent jobs into the store. Exported so the runs list can offer a retry.
 *
 *  🚨 A failure is recorded, not swallowed. SSE does deliver later updates, but it never delivers
 *  the jobs that already exist — and `loaded` is set by `setJobs` alone, so one failed seed left
 *  `/troubleshoot/runs` reading "Loading…" for as long as the app stayed open. */
export function seedAnalysisJobs(): void {
  const { setJobs, setLoadFailed } = useTroubleshootStore.getState();
  setLoadFailed(false);
  api
    .listAnalysisJobs(50)
    .then(setJobs)
    .catch(() => setLoadFailed(true));
}

export function useTroubleshootStream(): void {
  const upsertJob = useTroubleshootStore((s) => s.upsertJob);

  useEffect(() => {
    seedAnalysisJobs();
    return subscribeAnalysis(upsertJob);
  }, [upsertJob]);
}
