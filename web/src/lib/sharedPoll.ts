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
  let timer: ReturnType<typeof setInterval> | undefined;
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

  const start = () => {
    if (timer !== undefined || listeners.size === 0 || isHidden()) return;
    timer = setInterval(() => void load(), intervalMs);
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
    } else if (listeners.size > 0) {
      // Back on the tab: what is on screen is stale. Read at once, then resume.
      void load();
      start();
    }
  };

  const subscribe = (listener: () => void) => {
    listeners.add(listener);
    if (listeners.size === 1) {
      if (typeof document !== 'undefined') document.addEventListener('visibilitychange', onVisibility);
      if (!isHidden()) void load();
      start();
    }
    return () => {
      listeners.delete(listener);
      if (listeners.size === 0) {
        stop();
        if (typeof document !== 'undefined') {
          document.removeEventListener('visibilitychange', onVisibility);
        }
      }
    };
  };
  const snapshot = () => state;

  return () => useSyncExternalStore(subscribe, snapshot);
}
