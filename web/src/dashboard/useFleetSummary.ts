// Shared fleet status summary for the dashboard status widgets (status-summary, health-ring,
// nodes-down KPI). They all need the same server-computed per-state tally, so this dedupes it into
// one fetch + one 15s poll regardless of how many status-widgets are mounted (a subscriber count
// starts/stops the timer) — the same pattern as `useNodes`, but reading `/fleet/summary` so the
// numbers are correct over the WHOLE fleet, not the first page of `listNodes()` (S12).

import { useEffect } from 'react';
import { create } from 'zustand';
import { api } from '../services/api';
import type { FleetSummary } from '../types/api';

const REFRESH_MS = 15_000;

interface SummaryStore {
  summary: FleetSummary | null;
  loading: boolean;
  error: boolean;
  set: (patch: Partial<SummaryStore>) => void;
}

const useSummaryStore = create<SummaryStore>((set) => ({
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
    const summary = await api.getFleetSummary();
    useSummaryStore.getState().set({ summary, loading: false, error: false });
  } catch {
    useSummaryStore.getState().set({ loading: false, error: true });
  } finally {
    inFlight = false;
  }
}

let subscribers = 0;
let timer: ReturnType<typeof setInterval> | undefined;

/** Subscribe to the shared fleet summary (kept fresh on a 15s poll while any widget is mounted). */
export function useFleetSummary(): { summary: FleetSummary | null; loading: boolean; error: boolean } {
  const { summary, loading, error } = useSummaryStore();
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
