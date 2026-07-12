// Shared per-group health rollup for the site/topology dashboard widgets (site-matrix,
// region-rollup, geo-map). They all need the same server-computed per-group state tally, so this
// dedupes it into one fetch + one 15s poll regardless of how many are mounted (a subscriber count
// starts/stops the timer) — the same pattern as `useFleetSummary`, but reading `/fleet/group-summary`
// so the numbers cover the WHOLE fleet per group, not the first page of `listNodes()` (A-1).

import { useEffect } from 'react';
import { create } from 'zustand';
import { api } from '../services/api';
import type { FleetGroupSummary } from '../types/api';

const REFRESH_MS = 15_000;

interface GroupSummaryStore {
  summary: FleetGroupSummary | null;
  loading: boolean;
  error: boolean;
  set: (patch: Partial<GroupSummaryStore>) => void;
}

const useStore = create<GroupSummaryStore>((set) => ({
  summary: null,
  loading: true,
  error: false,
  set: (patch) => set(patch),
}));

let inFlight = false;
async function load(): Promise<void> {
  if (inFlight) return;
  inFlight = true;
  try {
    const summary = await api.getFleetGroupSummary();
    useStore.getState().set({ summary, loading: false, error: false });
  } catch {
    useStore.getState().set({ loading: false, error: true });
  } finally {
    inFlight = false;
  }
}

let subscribers = 0;
let timer: ReturnType<typeof setInterval> | undefined;

/** Subscribe to the shared per-group summary (kept fresh on a 15s poll while any widget is mounted). */
export function useGroupSummary(): {
  summary: FleetGroupSummary | null;
  loading: boolean;
  error: boolean;
} {
  const { summary, loading, error } = useStore();
  useEffect(() => {
    subscribers += 1;
    if (subscribers === 1) {
      void load();
      timer = setInterval(() => void load(), REFRESH_MS);
    }
    return () => {
      subscribers -= 1;
      if (subscribers === 0 && timer) {
        clearInterval(timer);
        timer = undefined;
      }
    };
  }, []);
  return { summary, loading, error };
}
