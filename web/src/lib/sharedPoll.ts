// SPDX-License-Identifier: AGPL-3.0-only
// One fetch, one timer, however many components read it (ADR-184).
//
// `useFleetSummary` and `useGroupSummary` were the same forty lines with two names changed: a
// zustand store, an in-flight flag, a subscriber count that starts the timer on the first mount and
// stops it on the last. This is that program once. What each caller supplies is the fetch; what it
// gets is a hook.
//
// It also stops while the tab is hidden and reads once on return — the rule `refreshTick.ts`
// already follows for node detail. A dashboard left open in a background tab used to keep polling
// the fleet summary every 15 seconds for as long as the browser lived.

import { useSyncExternalStore } from 'react';

/** The WebUI's standard refresh cadence: dashboards, node detail, System health. One number, so a
 *  change to it is one change. */
export const POLL_INTERVAL_MS = 15_000;

export interface SharedPolled<T> {
  data: T | null;
  /** True until the first answer (success or failure) arrives. */
  loading: boolean;
  /** The last read failed. `data` keeps the previous answer. */
  error: boolean;
}

/** Hidden tab (no document ⇒ the Vitest node environment ⇒ treated as visible). */
function isHidden(): boolean {
  return typeof document !== 'undefined' && document.visibilityState === 'hidden';
}

/** Run `run` every `intervalMs` while the tab is visible; returns the stop function.
 *
 *  The timer this module's shared polls, `usePolled` and `useLoad`'s `intervalMs` go through
 *  (ADR-184). Not every timer in the WebUI: a few screens still run their own `setInterval`
 *  (the node list, Upgrade, Relocation, the sync watch, `refreshTick`). It does **not** run `run` at start: each
 *  caller reads at once in its own way (on subscribe, on mount, on a dependency change), and a
 *  second immediate read here would be a duplicate request. What it does own is the hidden tab:
 *  the interval stops while hidden, and on return `run` fires at once — what is on screen is stale —
 *  and the interval resumes. */
export function pollWhileVisible(run: () => void, intervalMs: number): () => void {
  let timer: ReturnType<typeof setInterval> | undefined;
  const start = () => {
    if (timer === undefined && !isHidden()) timer = setInterval(run, intervalMs);
  };
  const stop = () => {
    if (timer !== undefined) {
      clearInterval(timer);
      timer = undefined;
    }
  };
  const onVisibility = () => {
    if (isHidden()) {
      stop();
    } else {
      run();
      start();
    }
  };
  if (typeof document !== 'undefined') document.addEventListener('visibilitychange', onVisibility);
  start();
  return () => {
    stop();
    if (typeof document !== 'undefined') {
      document.removeEventListener('visibilitychange', onVisibility);
    }
  };
}

/** Build a hook that shares one poll of `fetch` among every component that calls it.
 *
 *  - The first subscriber reads at once and starts the timer; the last one to leave stops it.
 *  - A tick that arrives while a read is still outstanding is dropped, not queued — a slow core
 *    must not accumulate a backlog of identical requests.
 *  - A failure keeps the previous `data` and sets `error`; the next success clears it. */
export function createSharedPoll<T>(
  fetch: () => Promise<T>,
  intervalMs: number = POLL_INTERVAL_MS,
): () => SharedPolled<T> {
  let state: SharedPolled<T> = { data: null, loading: true, error: false };
  const listeners = new Set<() => void>();
  let stopPolling: (() => void) | undefined;
  let inFlight = false;

  const publish = (next: SharedPolled<T>) => {
    state = next;
    for (const l of listeners) l();
  };

  const load = async () => {
    if (inFlight) return;
    inFlight = true;
    try {
      const data = await fetch();
      publish({ data, loading: false, error: false });
    } catch {
      publish({ ...state, loading: false, error: true });
    } finally {
      inFlight = false;
    }
  };

  const subscribe = (listener: () => void) => {
    listeners.add(listener);
    if (listeners.size === 1) {
      if (!isHidden()) void load();
      stopPolling = pollWhileVisible(() => void load(), intervalMs);
    }
    return () => {
      listeners.delete(listener);
      if (listeners.size === 0) {
        stopPolling?.();
        stopPolling = undefined;
      }
    };
  };
  const snapshot = () => state;

  return () => useSyncExternalStore(subscribe, snapshot);
}
