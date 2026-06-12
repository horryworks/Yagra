// Route table. Mirrors the nav IA (nav.ts): the AppShell wraps every in-app screen; static
// section/sub-feature paths map to their pages, and every not-yet-backed IA entry routes to
// ComingSoon so the structure is complete and navigable. Unknown paths fall back to the
// dashboard.

import { Navigate, Route, Routes } from 'react-router-dom';
import { AppShell } from './components/shell/AppShell';
import { ComingSoon } from './components/ui/ComingSoon';
import { LoginPage } from './pages/LoginPage';
import { DashboardPage } from './pages/DashboardPage';
import { NodesPage } from './pages/NodesPage';
import { NodeDetailPage } from './pages/NodeDetailPage';
import { ProfilesPage } from './pages/ProfilesPage';
import { CollectionTemplatesPage } from './pages/CollectionTemplatesPage';
import { MibRepositoryPage } from './pages/MibRepositoryPage';
import { DiscoveryPage } from './pages/DiscoveryPage';
import { CredentialsPage } from './pages/CredentialsPage';
import { ThresholdsPage } from './pages/ThresholdsPage';
import { ActiveAlertsPage } from './pages/ActiveAlertsPage';
import { HistoryPage } from './pages/HistoryPage';
import { RoutingPage } from './pages/RoutingPage';
import { MetricExplorerPage } from './pages/MetricExplorerPage';
import { PreferencesPage } from './pages/PreferencesPage';
import { UsersPage } from './pages/UsersPage';

export function AppRoutes() {
  return (
    <Routes>
      <Route path="/login" element={<LoginPage />} />

      <Route element={<AppShell />}>
        <Route index element={<Navigate to="/dashboard" replace />} />

        {/* Dashboard */}
        <Route path="dashboard" element={<DashboardPage />} />
        <Route path="dashboard/my" element={<ComingSoon />} />
        <Route path="dashboard/templates" element={<ComingSoon />} />

        {/* Nodes — static paths rank above the :nodeId dynamic segment in v6. */}
        <Route path="nodes" element={<NodesPage />} />
        <Route path="nodes/discovery" element={<DiscoveryPage />} />
        <Route path="nodes/dependencies" element={<ComingSoon />} />
        <Route path="nodes/profiles" element={<ProfilesPage />} />
        <Route path="nodes/collection-templates" element={<CollectionTemplatesPage />} />
        <Route path="nodes/mib" element={<MibRepositoryPage />} />
        <Route path="nodes/:nodeId" element={<NodeDetailPage />} />

        {/* Topology — all backend-pending */}
        <Route path="topology/map" element={<ComingSoon />} />
        <Route path="topology/dependency" element={<ComingSoon />} />
        <Route path="topology/geo" element={<ComingSoon />} />

        {/* Alerts */}
        <Route path="alerts" element={<ActiveAlertsPage />} />
        <Route path="alerts/history" element={<HistoryPage />} />
        <Route path="alerts/rules" element={<ThresholdsPage />} />
        <Route path="alerts/routing" element={<RoutingPage />} />
        <Route path="alerts/maintenance" element={<ComingSoon />} />
        <Route path="alerts/mutes" element={<ComingSoon />} />

        {/* Metrics */}
        <Route path="metrics" element={<MetricExplorerPage />} />
        <Route path="metrics/reports" element={<ComingSoon />} />
        <Route path="metrics/saved" element={<ComingSoon />} />

        {/* Settings */}
        <Route path="settings/pollers" element={<ComingSoon />} />
        <Route path="settings/integrations" element={<ComingSoon />} />
        <Route path="settings/credentials" element={<CredentialsPage />} />
        <Route path="settings/users" element={<UsersPage />} />
        <Route path="settings/auth" element={<ComingSoon />} />
        <Route path="settings/audit" element={<ComingSoon />} />
        <Route path="settings/system" element={<ComingSoon />} />
        <Route path="settings/preferences" element={<PreferencesPage />} />

        <Route path="*" element={<Navigate to="/dashboard" replace />} />
      </Route>
    </Routes>
  );
}
