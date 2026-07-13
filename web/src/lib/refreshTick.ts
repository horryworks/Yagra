// One shared live-refresh clock for the node-detail surfaces (S24). The Overview/Interfaces detail
// used to arm a separate `setInterval(load, 15_000)` per card — worst case ~7 independent, drifting
// timers on one node (status+RTT+interfaces orchestrator, CPU/mem/session/VPN health, URL/Meraki,
// the interface dock). This replaces all of them with a SINGLE module-level interval, ref-counted by
// subscriber count, exposed as a monotonic tick. A loader re-runs by listing the tick in its effect
// deps (`[nodeId, range, tick]`) instead of owning a timer, so N cards refresh in lockstep off one
// timer. It also pauses while the tab is hidden and refreshes immediately on return (a backgrounded
// detail no longer polls forever). Mirrors the useSyncExternalStore store shape in viewport.ts and
// the ref-counted single-subscription pattern in useNodeStates.ts.

import { useSyncExternalStore } from 'react';

/** The one live-refresh cadence for node-detail readings. Single source of truth (was declared
 *  independently in NodeDetail / OverviewTab / InterfacesTab). */
export const REFRESH_TICK_MS = 15_000;

let tick = 0;
let timer: ReturnType<typeof setInterval> | undefined;
const listeners = new Set<() => void>();
let visibilityBound = false;

function notify(): void {
  for (const l of listeners) l();
}

/** Advance the shared clock and wake every subscriber. Exported for tests. */
export function pumpRefreshTick(): void {
  tick += 1;
  notify();
}

/** Hidden tab (no document ⇒ SSR / the Vitest node env ⇒ treated as visible). */
function isHidden(): boolean {
  return typeof document !== 'undefined' && document.visibilityState === 'hidden';
}

function startTimer(): void {
  if (timer !== undefined || listeners.size === 0 || isHidden()) return;
  timer = setInterval(pumpRefreshTick, REFRESH_TICK_MS);
}

function stopTimer(): void {
  if (timer !== undefined) {
    clearInterval(timer);
    timer = undefined;
  }
}

function onVisibilityChange(): void {
  if (isHidden()) {
    stopTimer();
  } else if (listeners.size > 0) {
    // Back on the tab: the readings are stale — refresh at once, then resume ticking.
    pumpRefreshTick();
    startTimer();
  }
}

function bindVisibility(): void {
  if (visibilityBound || typeof document === 'undefined') return;
  document.addEventListener('visibilitychange', onVisibilityChange);
  visibilityBound = true;
}

/** Subscribe to the shared tick; the returned fn unsubscribes. The interval runs only while at least
 *  one subscriber is present (and the tab is visible). Exported so the hook and tests share it. */
export function subscribeRefreshTick(onChange: () => void): () => void {
  listeners.add(onChange);
  bindVisibility();
  startTimer();
  return () => {
    listeners.delete(onChange);
    if (listeners.size === 0) stopTimer();
  };
}

/** Current tick value (the useSyncExternalStore snapshot). */
export function getRefreshTick(): number {
  return tick;
}

/** Reset the shared clock (tests only). */
export function resetRefreshTick(): void {
  stopTimer();
  listeners.clear();
  tick = 0;
}

/**
 * Subscribe a component to the shared live-refresh tick. The number changes every `REFRESH_TICK_MS`
 * while mounted; list it in a data-loading effect's deps to re-fetch on each tick without owning a
 * timer:
 *
 *     const tick = useRefreshTick();
 *     useEffect(() => { load(); }, [nodeId, range, tick]);
 */
export function useRefreshTick(): number {
  return useSyncExternalStore(subscribeRefreshTick, getRefreshTick, () => 0);
}
