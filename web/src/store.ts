// SPDX-License-Identifier: AGPL-3.0-only
// Live alert state (Zustand). Ephemeral live data lives here, not in a persisted store
// (coding-conventions). Alerts are keyed by their dedup identity (node|check|severity) so
// an SSE re-delivery upserts rather than duplicates.

import { create } from 'zustand';
import { createJSONStorage, persist, type StateStorage } from 'zustand/middleware';
import { severityRank } from './lib/format';
import { getToken } from './services/api';
import type { Alert } from './types/api';
import { DEFAULT_RANGE, type Range } from './components/NodeDetail/RangeControl';

// sessionStorage when available (browser), else a no-op — keeps the store working in the Vitest
// node env (no sessionStorage) without a persist warning.
const sessionStore = (): StateStorage =>
  typeof sessionStorage !== 'undefined'
    ? sessionStorage
    : { getItem: () => null, setItem: () => undefined, removeItem: () => undefined };

// Shared authentication state so the app-level login gate and the Admin pane stay in
// sync (a single source of truth for "am I logged in"). The token itself lives in the
// api client (localStorage); this just tracks the boolean for re-rendering, plus the current
// principal's role (snake_case, e.g. 'admin') so role-gated UI (e.g. the Shared Dashboard
// customize control) can render synchronously without each consumer re-fetching /auth/me.
interface AuthStore {
  authed: boolean;
  /** Current principal's role (e.g. 'admin' | 'operator' | 'viewer'), or null when unknown/signed out. */
  role: string | null;
  setAuthed: (authed: boolean) => void;
  setRole: (role: string | null) => void;
}

export const useAuthStore = create<AuthStore>((set) => ({
  authed: getToken() != null,
  role: null,
  setAuthed: (authed) => set({ authed }),
  setRole: (role) => set({ role }),
}));

// Shared chart time-range so a selection made in one place (Overview Device health, the Interfaces
// dock, the Metric explorer) carries to the others across navigation — one source of truth for the
// active window. Persisted to sessionStorage so a browser reload restores the same window instead
// of snapping back to the default (design-guidelines.md "画面状態の永続化"); sessionStorage (not
// localStorage) scopes it to the tab/session, matching "reload shows the same view".
interface RangeStore {
  range: Range;
  setRange: (range: Range) => void;
}

export const useRangeStore = create<RangeStore>()(
  persist(
    (set) => ({
      range: DEFAULT_RANGE,
      setRange: (range) => set({ range }),
    }),
    { name: 'yagra.range', storage: createJSONStorage(sessionStore) },
  ),
);

// How tall the operator dragged the Geo map's pane. A layout preference, so it persists — snapping
// back to the default on every navigation is exactly the annoyance `design-guidelines.md`'s
// "画面状態の永続化" is about. localStorage rather than sessionStorage (unlike the chart range):
// this is a stable preference about how you like the page, not part of "reload shows the same
// view". `null` = never resized, so the page picks a height from the current window instead of
// pinning whatever the window happened to be on the day it was first opened.
interface MapPaneStore {
  geoHeight: number | null;
  setGeoHeight: (px: number) => void;
}

export const useMapPaneStore = create<MapPaneStore>()(
  persist(
    (set) => ({
      geoHeight: null,
      setGeoHeight: (geoHeight) => set({ geoHeight }),
    }),
    { name: 'yagra.mappane' },
  ),
);

export function alertKey(a: Pick<Alert, 'node' | 'check' | 'severity'>): string {
  return `${a.node}|${a.check}|${a.severity}`;
}

interface AlertStore {
  alerts: Record<string, Alert>;
  upsertAlert: (alert: Alert) => void;
  resolveAlert: (key: Pick<Alert, 'node' | 'check' | 'severity'>) => void;
  clear: () => void;
}

export const useAlertStore = create<AlertStore>((set) => ({
  alerts: {},
  upsertAlert: (alert) =>
    set((s) => ({ alerts: { ...s.alerts, [alertKey(alert)]: alert } })),
  resolveAlert: (key) =>
    set((s) => {
      const next = { ...s.alerts };
      delete next[alertKey(key)];
      return { alerts: next };
    }),
  clear: () => set({ alerts: {} }),
}));

/** Alerts sorted worst-first, then most-recent-first. */
export function sortedAlerts(alerts: Record<string, Alert>): Alert[] {
  return Object.values(alerts).sort(
    (a, b) =>
      severityRank(b.severity) - severityRank(a.severity) || b.at_unix_ms - a.at_unix_ms,
  );
}
