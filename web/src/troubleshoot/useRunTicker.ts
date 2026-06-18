// Drives the live feel of running jobs: ticks the store's progress every 1.4s while mounted.
// Mounted by the catalog and runs pages. In-app this would be replaced by the real job-status
// feed (poll or SSE — see services/sse.ts / hooks/useAlertStream.ts); the seam is the store.

import { useEffect } from 'react';
import { useTroubleshootStore } from './store';

export function useRunTicker(intervalMs = 1400) {
  const tickProgress = useTroubleshootStore((s) => s.tickProgress);
  useEffect(() => {
    const id = setInterval(tickProgress, intervalMs);
    return () => clearInterval(id);
  }, [tickProgress, intervalMs]);
}
