// Live alert state (Zustand). Ephemeral live data lives here, not in a persisted store
// (coding-conventions). Alerts are keyed by their dedup identity (node|check|severity) so
// an SSE re-delivery upserts rather than duplicates.

import { create } from 'zustand';
import { severityRank } from './lib/format';
import { getToken } from './services/api';
import type { Alert } from './types/api';

// Shared authentication state so the app-level login gate and the Admin pane stay in
// sync (a single source of truth for "am I logged in"). The token itself lives in the
// api client (localStorage); this just tracks the boolean for re-rendering.
interface AuthStore {
  authed: boolean;
  setAuthed: (authed: boolean) => void;
}

export const useAuthStore = create<AuthStore>((set) => ({
  authed: getToken() != null,
  setAuthed: (authed) => set({ authed }),
}));

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
