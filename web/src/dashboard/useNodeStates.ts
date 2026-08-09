// SPDX-License-Identifier: AGPL-3.0-only
// Live per-node display-state overrides, kept fresh via the node-state SSE stream (S14). One shared
// subscription (ref-counted, like `useFleetSummary`) feeds a Map that the inventory / topology /
// dashboard views overlay on top of their own base data — so a node's up/down/threshold state
// updates live without re-fetching the whole fleet every 15s. Base fetches still run (at a reduced
// cadence) and reconcile anything missed while a subscriber was lagged.
//
// Incoming events are coalesced: a full-sweep first-observe burst right after a core restart could
// otherwise be tens of thousands of events. We buffer them and rebuild the Map once per flush, so
// the work is O(nodes) per flush (not per event) and views re-render at most ~10×/s during a burst.

import { useEffect } from 'react';
import { create } from 'zustand';
import { subscribeNodeStates } from '../services/sse';
import type { NodeState } from '../types/api';

interface NodeStateStore {
  /** id → live display state. A fresh Map identity is published on each flush so `useMemo`
   *  consumers keyed on it recompute. */
  states: Map<string, NodeState>;
}

const useStore = create<NodeStateStore>(() => ({ states: new Map() }));

/** Slow reconcile cadence for views that keep node state live via SSE (S14): the periodic fetch now
 *  only catches structural/inventory changes (parent edges, new/removed nodes, root-cause) and
 *  reconciles any events missed while a subscriber was lagged — down from the old 15s full refetch. */
export const LIVE_RECONCILE_MS = 60_000;

const FLUSH_MS = 100;
let pending: Array<[string, NodeState]> = [];
let flushScheduled = false;

function flush(): void {
  flushScheduled = false;
  if (pending.length === 0) return;
  const next = new Map(useStore.getState().states);
  let changed = false;
  for (const [id, state] of pending) {
    if (next.get(id) !== state) {
      next.set(id, state);
      changed = true;
    }
  }
  pending = [];
  if (changed) useStore.setState({ states: next });
}

/** Buffer one live state change; a flush is scheduled to apply the batch. Exported for tests. */
export function ingestNodeState(id: string, state: NodeState): void {
  pending.push([id, state]);
  if (!flushScheduled) {
    flushScheduled = true;
    setTimeout(flush, FLUSH_MS);
  }
}

/**
 * Reset the shared map + pending buffer.
 *
 * **Test infrastructure — it has no production caller and is not supposed to gain one.** The store
 * and the pending buffer are module-level, so they outlive a single test; without this every case
 * in `useNodeStates.test.ts` would inherit the previous one's Map and the flush-coalescing
 * assertions would pass or fail depending on file order. Nothing in the app resets live node state:
 * a reload remounts the module. So a dead-code sweep will keep finding it — it is deliberate, not
 * a leftover.
 */
export function resetNodeStates(): void {
  pending = [];
  useStore.setState({ states: new Map() });
}

let subscribers = 0;
let unsub: (() => void) | undefined;

/**
 * Subscribe to the shared live node-state map (one SSE connection while any view is mounted).
 * Overlay it as `live.get(id) ?? base.state` on top of a node's fetched state.
 */
export function useNodeStates(): Map<string, NodeState> {
  const states = useStore((s) => s.states);
  useEffect(() => {
    subscribers += 1;
    if (subscribers === 1) {
      unsub = subscribeNodeStates((ev) => ingestNodeState(ev.node_id, ev.state));
    }
    return () => {
      subscribers -= 1;
      if (subscribers === 0 && unsub) {
        unsub();
        unsub = undefined;
      }
    };
  }, []);
  return states;
}
