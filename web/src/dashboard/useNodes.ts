// Shared node inventory for dashboard widgets. Many widgets (status summary, health ring,
// site matrix, region rollup, KPI tile) need the same `listNodes()` result, so this dedupes
// it into one fetch + one 15s poll regardless of how many node-widgets are mounted (a simple
// subscriber count starts/stops the timer). Ephemeral live data → non-persisted store.

import { useEffect } from 'react';
import { create } from 'zustand';
import { api } from '../services/api';
import type { NodeSummary } from '../types/api';

const REFRESH_MS = 15_000;

interface NodesStore {
  nodes: NodeSummary[];
  loading: boolean;
  error: boolean;
  set: (patch: Partial<NodesStore>) => void;
}

const useNodesStore = create<NodesStore>((set) => ({
  nodes: [],
  loading: true,
  error: false,
  set: (patch) => set(patch),
}));

let inFlight = false;
async function load(): Promise<void> {
  if (inFlight) return;
  inFlight = true;
  try {
    const nodes = await api.listNodes();
    useNodesStore.getState().set({ nodes, loading: false, error: false });
  } catch {
    useNodesStore.getState().set({ loading: false, error: true });
  } finally {
    inFlight = false;
  }
}

let subscribers = 0;
let timer: ReturnType<typeof setInterval> | undefined;

/** Subscribe to the shared node list (kept fresh on a 15s poll while any widget is mounted). */
export function useNodes(): { nodes: NodeSummary[]; loading: boolean; error: boolean } {
  const { nodes, loading, error } = useNodesStore();
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
  return { nodes, loading, error };
}
