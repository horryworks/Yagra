// Route table. Mirrors the nav IA (nav.ts): the AppShell wraps every in-app screen; static
// section/sub-feature paths map to their pages, and every not-yet-backed IA entry routes to
// ComingSoon so the structure is complete and navigable. Unknown paths fall back to the
// dashboard.

import { Navigate, Route, Routes } from 'react-router-dom';
import { AppShell } from './components/shell/AppShell';
import { ComingSoon } from './components/ui/ComingSoon';
import { LoginPage } from './pages/LoginPage';
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
import { CredentialsPage } from './pages/CredentialsPage';
import { ThresholdsPage } from './pages/ThresholdsPage';
import { ActiveAlertsPage } from './pages/ActiveAlertsPage';
import { HistoryPage } from './pages/HistoryPage';
import { RoutingPage } from './pages/RoutingPage';
import { MaintenancePage } from './pages/MaintenancePage';
import { MutesPage } from './pages/MutesPage';
import { AuditPage } from './pages/AuditPage';
import { PreferencesPage } from './pages/PreferencesPage';
import { SystemHealthPage } from './pages/SystemHealthPage';
import { IntegrationsPage } from './pages/IntegrationsPage';
import { SystemSettingsPage } from './pages/SystemSettingsPage';
import { AboutPage } from './pages/AboutPage';
import { UsersPage } from './pages/UsersPage';
import { RolesPage } from './pages/RolesPage';
import { TroubleshootCatalogPage } from './troubleshoot/TroubleshootCatalogPage';
import { RunsPage } from './troubleshoot/RunsPage';
import { AnomalyReportPage } from './troubleshoot/AnomalyReportPage';

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
        <Route path="nodes/dependencies" element={<ComingSoon />} />
        <Route path="nodes/profiles" element={<ProfilesPage />} />
        <Route path="nodes/classification-rules" element={<ClassificationRulesPage />} />
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
        <Route path="alerts/maintenance" element={<MaintenancePage />} />
        <Route path="alerts/mutes" element={<MutesPage />} />

        {/* Troubleshoot — deep diagnostics run as background jobs */}
        <Route path="troubleshoot" element={<TroubleshootCatalogPage />} />
        <Route path="troubleshoot/runs" element={<RunsPage />} />
        <Route path="troubleshoot/anomaly" element={<AnomalyReportPage />} />
        <Route path="troubleshoot/scheduled" element={<ComingSoon />} />
        <Route path="troubleshoot/findings" element={<ComingSoon />} />

        {/* Settings */}
        <Route path="settings/system-health" element={<SystemHealthPage />} />
        <Route path="settings/pollers" element={<ComingSoon />} />
        <Route path="settings/integrations" element={<IntegrationsPage />} />
        <Route path="settings/credentials" element={<CredentialsPage />} />
        <Route path="settings/users" element={<UsersPage />} />
        <Route path="settings/roles" element={<RolesPage />} />
        <Route path="settings/auth" element={<ComingSoon />} />
        <Route path="settings/audit" element={<AuditPage />} />
        <Route path="settings/system" element={<SystemSettingsPage />} />
        <Route path="settings/preferences" element={<PreferencesPage />} />
        <Route path="settings/about" element={<AboutPage />} />

        <Route path="*" element={<Navigate to="/dashboard" replace />} />
      </Route>
    </Routes>
  );
}
