// SPDX-License-Identifier: AGPL-3.0-only
// Lazy route group: Settings — sixteen administration screens (Yagra health, pollers, integrations,
// AI, users/roles/auth/TLS/tokens, audit, monitoring defaults, config bundle, support bundle,
// upgrade, preferences, about). Mounted by `routes.tsx` at `settings/*` through `React.lazy`: this
// is the largest group by far and almost none of it is on an operator's daily path.
//
// Two screens left in ADR-055 Inc.2 and their old paths redirect from `routes.tsx`: Forwarding to
// the Events tab (it relays received passive data, and the rest of that pipeline was already
// there), and Credentials to Nodes (device keys are part of collection, not of who may sign in).
//
// One boundary for the whole group: the group component stays mounted across its screens, so only
// the first entry suspends and moving between settings tabs never flashes the fallback. See
// `topology.tsx` for the rest of the reasoning, including why the trailing `*` is required.
//
// `/settings` itself redirects from `routes.tsx` (statically) so a bare visit lands on System
// health without paying for this chunk first; the index route here covers the trailing-slash form.

import { Navigate, Route, Routes } from 'react-router-dom';
import { SystemHealthPage } from '../pages/SystemHealthPage';
import { PollersPage } from '../pages/PollersPage';
import { IntegrationsCatalogPage } from '../pages/integrations/IntegrationsCatalogPage';
import { MerakiIntegrationPage } from '../pages/integrations/MerakiIntegrationPage';
import { AiSettingsPage } from '../pages/AiSettingsPage';
import { UsersPage } from '../pages/UsersPage';
import { RolesPage } from '../pages/RolesPage';
import { AuthSettingsPage } from '../pages/AuthSettingsPage';
import { TlsSettingsPage } from '../pages/TlsSettingsPage';
import { ApiTokensPage } from '../pages/ApiTokensPage';
import { AuditPage } from '../pages/AuditPage';
import { SystemSettingsPage } from '../pages/SystemSettingsPage';
import { ConfigBundlePage } from '../pages/ConfigBundlePage';
import { SupportBundlePage } from '../pages/SupportBundlePage';
import { UpgradePage } from '../pages/UpgradePage';
import { AboutPage } from '../pages/AboutPage';

export default function SettingsRoutes() {
  return (
    <Routes>
      {/* The tab lands on System health (the first sidebar item). */}
      <Route index element={<Navigate to="/settings/system-health" replace />} />
      <Route path="system-health" element={<SystemHealthPage />} />
      <Route path="pollers" element={<PollersPage />} />
      <Route path="integrations" element={<IntegrationsCatalogPage />} />
      <Route path="integrations/meraki" element={<MerakiIntegrationPage />} />
      <Route path="ai" element={<AiSettingsPage />} />
      <Route path="users" element={<UsersPage />} />
      <Route path="roles" element={<RolesPage />} />
      <Route path="auth" element={<AuthSettingsPage />} />
      <Route path="tls" element={<TlsSettingsPage />} />
      <Route path="api-tokens" element={<ApiTokensPage />} />
      <Route path="audit" element={<AuditPage />} />
      <Route path="system" element={<SystemSettingsPage />} />
      <Route path="config-bundle" element={<ConfigBundlePage />} />
      <Route path="support-bundle" element={<SupportBundlePage />} />
      <Route path="upgrade" element={<UpgradePage />} />
      <Route path="about" element={<AboutPage />} />
      <Route path="*" element={<Navigate to="/dashboard" replace />} />
    </Routes>
  );
}
