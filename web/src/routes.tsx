// SPDX-License-Identifier: AGPL-3.0-only
// Route table. Mirrors the nav IA (nav.ts): the AppShell wraps every in-app screen and static
// section/sub-feature paths map to their pages. Unknown paths fall back to the dashboard.
//
// Every IA entry now has a real screen, so nothing routes to `ComingSoon` at the moment. The
// component and `nav.ts`'s `implemented` flag stay: they are the mechanism for slotting a screen
// into the IA before it has a backend, and `nav.test.ts` still pins how a placeholder is grouped.
//
// CODE SPLITTING. Three route GROUPS are lazy — Topology, Troubleshoot, Settings (`routeGroups/`).
// Everything on an operator's daily path (dashboard, nodes, alerts) is imported statically and
// stays in the initial chunk; the ~25 screens behind those three sections are not, so opening the
// dashboard no longer downloads the geo outline, the report registry and seventeen settings pages.
// The split is per GROUP and not per page on purpose: a group's component stays mounted while the
// operator moves between its screens, so only the first entry suspends and a tab switch inside an
// already-loaded group never flashes the fallback. React.lazy also caches the resolved module, so
// leaving a group and coming back is synchronous too.

import { lazy, Suspense, useEffect } from 'react';
import { Navigate, Outlet, Route, Routes } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { AppShell } from './components/shell/AppShell';
import { MovedTo } from './components/shell/MovedTo';
import { LoginPage } from './pages/LoginPage';
import { usePrefsDialogStore } from './store';
import { SharedDashboardPage } from './dashboard/SharedDashboardPage';
import { MyDashboardPage } from './dashboard/MyDashboardPage';
import { ReportsPage } from './reports/ReportsPage';
import { NodesPage } from './pages/NodesPage';
import { NodeDetailPage } from './pages/NodeDetailPage';
import { ProfilesPage } from './pages/ProfilesPage';
import { ClassificationRulesPage } from './pages/ClassificationRulesPage';
import { CollectionTemplatesPage } from './pages/CollectionTemplatesPage';
import { MibRepositoryPage } from './pages/MibRepositoryPage';
import { DiscoveryPage } from './pages/DiscoveryPage';
import { ThresholdsPage } from './pages/ThresholdsPage';
import { ActiveAlertsPage } from './pages/ActiveAlertsPage';
import { HistoryPage } from './pages/HistoryPage';
import { RoutingPage } from './pages/RoutingPage';
import { EventsPage } from './pages/EventsPage';
import { EventRulesPage } from './pages/EventRulesPage';
import { EventSourcesPage } from './pages/EventSourcesPage';
import { MaintenancePage } from './pages/MaintenancePage';
import { MutesPage } from './pages/MutesPage';

const TopologyRoutes = lazy(() => import('./routeGroups/topology'));
const TroubleshootRoutes = lazy(() => import('./routeGroups/troubleshoot'));
const SettingsRoutes = lazy(() => import('./routeGroups/settings'));
const EventsRoutes = lazy(() => import('./routeGroups/events'));

/** Credentials, lazily — a group of one, and the only per-PAGE split here.
 *
 *  ADR-055 moved it out of Settings and into Nodes, where it belongs by subject (they are keys for
 *  reaching devices, not for signing in to Yagra). Nodes has no lazy group, and the reason the rest
 *  of the split is per-group — a group stays mounted so moving between its screens never re-suspends
 *  — has nothing to say about a screen with no siblings. It stays lazy because it is 685 lines of
 *  configuration form on a tab an operator otherwise opens all day: changing which tab owns it is
 *  no reason to grow the initial chunk. */
const CredentialsPage = lazy(() =>
  import('./pages/CredentialsPage').then((m) => ({ default: m.CredentialsPage })),
);

/** The address Preferences had before ADR-055 Inc.7 made it a dialog. Falling through to the
 *  settings splat's catch-all would redirect to the dashboard with nothing to show for it, so a
 *  bookmark would look like the screen had been deleted. Open the dialog on the way instead. */
function OpenPreferences() {
  const setPrefsOpen = usePrefsDialogStore((s) => s.setOpen);
  useEffect(() => {
    setPrefsOpen(true);
  }, [setPrefsOpen]);
  return <Navigate to="/dashboard" replace />;
}

/** Layout route for the lazy groups: holds the one Suspense boundary, inside the AppShell so the
 *  top bar and sidebar stay put while a group's chunk arrives. The fallback is the app's existing
 *  loading treatment (the same markup `App.tsx` shows while the client config resolves). */
function LazyGroup() {
  const { t } = useTranslation();
  return (
    <Suspense fallback={<div className="app-loading muted">{t('loading')}</div>}>
      <Outlet />
    </Suspense>
  );
}

export function AppRoutes() {
  return (
    <Routes>
      <Route path="/login" element={<LoginPage />} />

      <Route element={<AppShell />}>
        <Route index element={<Navigate to="/dashboard" replace />} />

        {/* Dashboard */}
        <Route path="dashboard" element={<SharedDashboardPage />} />
        <Route path="dashboard/my" element={<MyDashboardPage />} />
        <Route path="dashboard/reports" element={<ReportsPage />} />

        {/* Nodes — static paths rank above the :nodeId dynamic segment in v6. */}
        <Route path="nodes" element={<NodesPage />} />
        <Route path="nodes/discovery" element={<DiscoveryPage />} />
        {/* Dependencies live under Topology now; keep this path as a redirect so old links/bookmarks
            resolve (and don't get captured by the nodes/:nodeId dynamic segment below). */}
        <Route
          path="nodes/dependencies"
          element={<Navigate to="/topology/dependency" replace />}
        />
        <Route path="nodes/profiles" element={<ProfilesPage />} />
        <Route path="nodes/classification-rules" element={<ClassificationRulesPage />} />
        <Route path="nodes/collection-templates" element={<CollectionTemplatesPage />} />
        <Route path="nodes/mib" element={<MibRepositoryPage />} />
        <Route path="nodes/:nodeId" element={<NodeDetailPage />} />

        {/* Alerts */}
        <Route path="alerts" element={<ActiveAlertsPage />} />
        <Route path="alerts/history" element={<HistoryPage />} />
        <Route path="alerts/rules" element={<ThresholdsPage />} />
        <Route path="alerts/routing" element={<RoutingPage />} />
        <Route path="alerts/event-rules" element={<EventRulesPage />} />
        <Route path="alerts/maintenance" element={<MaintenancePage />} />
        <Route path="alerts/mutes" element={<MutesPage />} />

        {/* Events (ADR-055 Inc.2). The log itself and the webhook list stay statically imported —
            the log is on an operator's daily path, and both already were. Forwarding is the one
            configuration-weight screen here (774 lines) and is lazy in the group below, exactly as
            it was under Settings; moving tabs is no reason to grow the initial chunk. */}
        <Route path="events" element={<EventsPage />} />
        <Route path="events/webhooks" element={<EventSourcesPage />} />

        {/* Where those four screens used to live. `MovedTo` rather than `Navigate` because
            `/alerts/events?node_id=…` is a real link (the node's Events tab emits it) and a bare
            redirect drops the query, landing on every node's events with nothing to show for it. */}
        <Route path="alerts/events" element={<MovedTo to="/events" />} />
        <Route path="alerts/event-sources" element={<MovedTo to="/events/webhooks" />} />
        <Route path="settings/forwarding" element={<MovedTo to="/events/forwarding" />} />
        <Route path="settings/credentials" element={<MovedTo to="/nodes/credentials" />} />
        {/* Not `MovedTo`: there is no page to move to. A static segment, so it outranks the
            `settings/*` splat below and never fetches the settings chunk. */}
        <Route path="settings/preferences" element={<OpenPreferences />} />

        {/* Settings lands on System health (the first sidebar item). Kept here rather than inside
            the group so a bare `/settings` redirects without fetching the settings chunk — a
            static segment outranks the `settings/*` splat below. */}
        <Route path="settings" element={<Navigate to="/settings/system-health" replace />} />

        {/* The lazy groups. Each mounts its own nested <Routes>; see `routeGroups/`.
            Topology — the network map visualizes the derived connectivity graph
            (GET /api/v1/topology), the dependency view manages its edges, and the geo map places
            groups by the coordinates `PUT /api/v1/node-groups/{id}/geo` stores.
            Troubleshoot — deep diagnostics run as background jobs. */}
        <Route element={<LazyGroup />}>
          <Route path="topology/*" element={<TopologyRoutes />} />
          <Route path="troubleshoot/*" element={<TroubleshootRoutes />} />
          <Route path="settings/*" element={<SettingsRoutes />} />
          {/* Events — only Forwarding is behind this; the two static `events` paths above outrank
              the splat, the same way `settings` does. */}
          <Route path="events/*" element={<EventsRoutes />} />
          <Route path="nodes/credentials" element={<CredentialsPage />} />
        </Route>

        <Route path="*" element={<Navigate to="/dashboard" replace />} />
      </Route>
    </Routes>
  );
}
