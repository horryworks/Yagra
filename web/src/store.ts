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
import { normalizeNodeDetailTab, type NodeDetailTab } from './components/NodeDetail/tabs';

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

// The node-detail sub-tab the operator last *clicked* (ADR-134). The URL's `?tab=` stays the source
// of truth; this is only the default a host uses when the URL names no tab — which is every arrival
// that is not a reload or a shared link: the inventory split deletes `tab` when a new row is picked,
// and every `navigate('/nodes/<id>')` in the app carries no query at all. So without this, comparing
// the same tab across a stack of switches means re-clicking it on every one. Read it through
// `requestedNodeDetailTab` (components/NodeDetail/tabs.ts), never as a default of its own.
//
// 🚨 **Only a click writes here.** `NodeDetail`'s correction effect rewrites a tab the loaded node
// does not offer, and it must NOT record that: walking a row of switches on Interfaces with one URL
// monitor among them would otherwise leave the memory on Overview, so the memory would mean "the
// last screen I was dropped onto" rather than "the last one I chose".
//
// sessionStorage, like the chart range above: this is part of "what am I looking at", not a standing
// preference, and it is deliberately not on the account (ADR-134 決定 4 — one PUT and one audit row
// per tab click, which is not a rate a debounce can fold).
interface NodeTabStore {
  tab: NodeDetailTab;
  /** Record a tab the operator clicked. Normalized on the way in so a stale session value from a
   *  build that had a tab this one does not cannot pin the whole app to Overview-by-fallback. */
  rememberTab: (tab: string) => void;
}

export const useNodeTabStore = create<NodeTabStore>()(
  persist(
    (set) => ({
      tab: 'overview',
      rememberTab: (tab) => set({ tab: normalizeNodeDetailTab(tab) }),
    }),
    { name: 'yagra.nodetab', storage: createJSONStorage(sessionStore) },
  ),
);

// Which board each dashboard was last showing (ADR-134). The boards themselves are server-persisted
// (`user_dashboards` / `shared_dashboard`); only the *pointer* was ephemeral, and `load()` runs on
// every mount — so a multi-board operator was put back on board 1 every time they returned to
// /dashboard. Keyed by which dashboard it is, because the three are separate documents with
// separate board sets and one shared key would name a board the other two do not have.
//
// Not in the URL: the sidebar navigates to a bare `/dashboard`, so a query parameter would be
// dropped by the very navigation this exists to survive. Not in the saved document either — that
// would make "which board am I looking at" a thing other sessions and other machines vote on.
interface LastBoardStore {
  /** Board id per dashboard key; a key absent means "never switched", so the first board wins. */
  byBoard: Record<string, string>;
  rememberBoard: (key: string, id: string) => void;
}

export const useLastBoardStore = create<LastBoardStore>()(
  persist(
    (set) => ({
      byBoard: {},
      rememberBoard: (key, id) =>
        set((s) => (s.byBoard[key] === id ? s : { byBoard: { ...s.byBoard, [key]: id } })),
    }),
    { name: 'yagra.lastboard', storage: createJSONStorage(sessionStore) },
  ),
);

/** A map's pan/zoom — the shape `TopologyMap`'s `View` and `GeoMapPage`'s `GeoView` share. */
export interface MapView {
  tx: number;
  ty: number;
  scale: number;
}

/** Which map a stored view belongs to. Two maps, two memories: they project different things. */
export type MapViewKey = 'topo' | 'geo';

// Where each map was panned and zoomed to (ADR-134). Both maps already work hard *not* to lose this
// within one mount — the `view === null` guard is what stops their 15s refresh from stomping the
// operator's pan every tick — and that care stopped at the component boundary: stepping to a node
// and back re-fitted the whole diagram.
//
// ⚠️ **These are container pixels, not geography** (ADR-134 決定 7). Restored at a different window
// width the view is off; it is worth carrying anyway because "Fit to view" puts it right in one
// click, while re-zooming every visit has no such fix. `null` = never moved, so the first paint
// still auto-fits.
interface MapViewStore {
  topo: MapView | null;
  geo: MapView | null;
  /** Set one map's view. Accepts an updater so a gesture can read the live value, and resolves it
   *  **here** rather than in the hook — a judgement inside a `.tsx` hook is one no test can run. */
  setMapView: (
    key: MapViewKey,
    next: MapView | null | ((prev: MapView | null) => MapView | null),
  ) => void;
}

export const useMapViewStore = create<MapViewStore>()(
  persist(
    (set) => ({
      topo: null,
      geo: null,
      setMapView: (key, next) =>
        set((s) => ({ [key]: typeof next === 'function' ? next(s[key]) : next }) as Partial<MapViewStore>),
    }),
    { name: 'yagra.mapview', storage: createJSONStorage(sessionStore) },
  ),
);

// Where each nav section was last visited (ADR-134 増分 2). The top-bar tab used to navigate to
// `NavSection.path`, a constant — so Dashboard always opened Shared dashboard even for an operator
// who had spent the morning on their own board, and Settings always opened System health out of
// sixteen screens. Read it through `sectionLandingPath` (nav.ts), which is where a stored value is
// validated; write it through `rememberableRoute`, which is where "only a screen the menu declares"
// lives, so a node detail never becomes the Nodes tab's destination.
//
// sessionStorage, like the three above: part of "what am I looking at", not a standing preference,
// and deliberately not on the account (決定 11 — one PUT and one audit row per navigation).
interface SectionRouteStore {
  /** Section key → the last route visited in it (`/dashboard/my`, `/events?message=router`).
   *  A key absent means "never visited", so that section's landing child wins. */
  bySection: Record<string, string>;
  rememberSectionRoute: (key: string, route: string) => void;
}

export const useSectionRouteStore = create<SectionRouteStore>()(
  persist(
    (set) => ({
      bySection: {},
      // Same value ⇒ same state object, so nothing re-renders and nothing is written. A filter can
      // rewrite the URL on every keystroke, and this effect runs on every one of them.
      rememberSectionRoute: (key, route) =>
        set((s) =>
          s.bySection[key] === route ? s : { bySection: { ...s.bySection, [key]: route } },
        ),
    }),
    { name: 'yagra.navroute', storage: createJSONStorage(sessionStore) },
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
