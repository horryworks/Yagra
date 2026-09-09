// SPDX-License-Identifier: AGPL-3.0-only
// Live alert state (Zustand). Ephemeral live data lives here, not in a persisted store
// (coding-conventions). Alerts are keyed by their dedup identity (node|check|severity) so
// an SSE re-delivery upserts rather than duplicates.

import { create } from 'zustand';
import { createJSONStorage, persist, type StateStorage } from 'zustand/middleware';
import { severityRank } from './lib/format';
import { grants, permissionLabel } from './lib/permissions';
import { getToken, type ClientConfig } from './services/api';
import type { Viewer } from './dashboard/layoutAccess';
import type { Alert, Permission, RoleMatrix, Scope, UserKind } from './types/api';
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
  /** Current principal's visibility scope, or null when unknown/signed out. Held because a scoped
   *  account otherwise has no way to tell a filtered inventory from a small one — every list it
   *  sees is simply shorter, with nothing on screen saying why. */
  scope: Scope | null;
  /** How the signed-in account authenticates, or null while it is resolving, when signed out,
   *  or when this core has no user store to ask. **Read it through `hasLocalPassword` /
   *  `passwordHomeKey` (`lib/password.ts`), never by comparing it here** — a second copy of
   *  "which kinds have a password Yagra holds" is a second answer (ADR-122 決定 5). */
  accountKind: UserKind | null;
  /** The server's role/privilege matrix (`GET /api/v1/roles`), or null while it is still
   *  resolving. Held whole rather than pre-reduced to this principal's permission list so that
   *  "may I?" and "what is this privilege called?" read the same single source — and so nothing
   *  here is a second copy of `rbac.rs`. */
  roleMatrix: RoleMatrix | null;
  setAuthed: (authed: boolean) => void;
  setRole: (role: string | null) => void;
  setScope: (scope: Scope | null) => void;
  setAccountKind: (kind: UserKind | null) => void;
  setRoleMatrix: (matrix: RoleMatrix | null) => void;
}

export const useAuthStore = create<AuthStore>((set) => ({
  authed: getToken() != null,
  role: null,
  scope: null,
  accountKind: null,
  roleMatrix: null,
  setAuthed: (authed) => set({ authed }),
  setRole: (role) => set({ role }),
  setScope: (scope) => set({ scope }),
  setAccountKind: (accountKind) => set({ accountKind }),
  setRoleMatrix: (roleMatrix) => set({ roleMatrix }),
}));

// ── The deployment's own client config (`GET /api/v1/config`) ────────────────────────────────
//
// Held in a store rather than in `App.tsx`'s local state because three things now branch on it: the
// login gate, the anonymous shell, and every layout store's decision about whether there is a row
// to fetch. Passing it down would mean threading it through the dashboard stores, which are created
// at module scope.

/** How far the config fetch has got. `unreachable` is a state, not a value — see [`useConfigStore`]. */
export type ConfigStatus = 'loading' | 'ready' | 'unreachable';

interface ConfigStore {
  status: ConfigStatus;
  config: ClientConfig | null;
  setConfig: (config: ClientConfig) => void;
  setUnreachable: () => void;
}

/**
 * The deployment's client config, and whether we could read it.
 *
 * 🚨 **`unreachable` must never be treated as "public".** `App.tsx` used to `.catch()` the fetch and
 * substitute `{ public_dashboard: true }`, so a core that was down — or upgrading, or misconfigured
 * — put every visitor straight into the app shell with no login screen and every panel erroring.
 * Nothing leaked (the API answers 401 regardless), but the screen was wrong in the one moment an
 * operator most needs it to be right. Unknown is closed: see `appGate.ts`.
 */
export const useConfigStore = create<ConfigStore>((set) => ({
  status: 'loading',
  config: null,
  setConfig: (config) => set({ config, status: 'ready' }),
  setUnreachable: () => set({ status: 'unreachable' }),
}));

/** What the dashboard stores need to know about the viewer (ADR-123). Not a hook: the layout stores
 *  are created at module scope and read this from outside React, in `load()`. */
export function currentViewer(): Viewer {
  return {
    hasSession: getToken() != null,
    publicDashboard: useConfigStore.getState().config?.public_dashboard === true,
  };
}

/**
 * Whether the current principal may do `perm` — the hook every write control asks before drawing
 * itself (ADR-056 Inc.2).
 *
 * Ask for the permission the *action* requires, never for a role: `useCan('manage_maintenance')`,
 * not `role === 'admin'`. The mapping from role to permission is the server's and this reads its
 * answer, so a privilege that moves between roles moves in one place. Answers false while the
 * matrix is still resolving, so a control appears a moment late rather than appearing and failing.
 */
export function useCan(perm: Permission): boolean {
  return useAuthStore((s) => grants(s.roleMatrix, s.role, perm));
}

/**
 * The server's own name for a privilege, for the screens that have to say which one is missing.
 * Falls back to the permission key before the matrix arrives.
 */
export function usePermissionLabel(perm: Permission): string {
  return useAuthStore((s) => permissionLabel(s.roleMatrix, perm));
}

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

// Whether the Preferences dialog is open (ADR-055 Inc.7). Shell-wide rather than local to
// `UserMenu` because two unrelated things open it: the account badge's menu item, and a visit to
// `/settings/preferences` — the address the screen had before it became a dialog, which a bookmark
// should not turn into a blank redirect. Ephemeral, so no `persist`.
interface PrefsDialogStore {
  open: boolean;
  setOpen: (open: boolean) => void;
}

export const usePrefsDialogStore = create<PrefsDialogStore>((set) => ({
  open: false,
  setOpen: (open) => set({ open }),
}));
