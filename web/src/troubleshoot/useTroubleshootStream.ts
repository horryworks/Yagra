// Seeds the analysis-runs list once, then keeps it live over SSE (ADR-022) — the analysis analog
// of useAlertStream. Mounted by the catalog and runs pages; the sidebar badge reads the store.

import { useEffect } from 'react';
import { api } from '../services/api';
import { subscribeAnalysis } from '../services/sse';
import { useTroubleshootStore } from './store';

export function useTroubleshootStream(): void {
  const setJobs = useTroubleshootStore((s) => s.setJobs);
  const upsertJob = useTroubleshootStore((s) => s.upsertJob);

  useEffect(() => {
    // Seed: fetch recent jobs once (tolerates transient failure — SSE still delivers updates).
    api
      .listAnalysisJobs(50)
      .then(setJobs)
      .catch(() => {
        /* transient / gated */
      });
    return subscribeAnalysis(upsertJob);
  }, [setJobs, upsertJob]);
}
