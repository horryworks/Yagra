// Generic polled-fetch hook for dashboard widgets that read a snapshot endpoint (maintenance
// windows, audit, discovery, alert history, Top-N). Fetches on mount and re-fetches every
// `intervalMs` (default 15s, matching the dashboard cadence), cancelling in-flight updates on
// unmount/dep-change so a slow response can't write into a torn-down widget.

import { useEffect, useState } from 'react';
import i18n from '../i18n';
import { ApiError } from '../services/api';

export interface Polled<T> {
  data: T | null;
  loading: boolean;
  /** A human-readable error message, or null. */
  error: string | null;
}

const REFRESH_MS = 15_000;

/** Poll `fetcher` on mount and every `intervalMs`, re-arming whenever `deps` change.
 *
 *  CONTRACT — avoid a stale closure: every value the `fetcher` closes over (query args, props,
 *  state) MUST appear in `deps`. An inline arrow is fine — pass `[]` only when the fetcher
 *  captures nothing that changes. Passing `[]` while the fetcher reads a changing prop/state
 *  would keep polling with the first render's value. (e.g. `() => api.getTopMetrics(metric,
 *  { agg })` needs `deps: [metric, agg]`.) */
export function usePolled<T>(
  fetcher: () => Promise<T>,
  deps: readonly unknown[] = [],
  intervalMs: number = REFRESH_MS,
): Polled<T> {
  const [state, setState] = useState<Polled<T>>({ data: null, loading: true, error: null });

  useEffect(() => {
    let cancelled = false;
    const run = () => {
      fetcher()
        .then((data) => {
          if (!cancelled) setState({ data, loading: false, error: null });
        })
        .catch((e: unknown) => {
          if (!cancelled) {
            setState((s) => ({
              ...s,
              loading: false,
              error: e instanceof ApiError ? e.message : i18n.t('dashboard:err.requestFailed'),
            }));
          }
        });
    };
    run();
    const id = setInterval(run, intervalMs);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);

  return state;
}
