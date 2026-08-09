// SPDX-License-Identifier: AGPL-3.0-only
// Lazy route group: Troubleshoot (catalog, runs, the parameterized report route and its registry,
// scheduled runs, saved findings). Mounted by `routes.tsx` at `troubleshoot/*` through `React.lazy`
// so the report registry and its charts stay out of the initial chunk.
//
// The SSE stream and its completion toast are NOT here — they live on the AppShell, because a
// "notify me" run must still report back after the operator has navigated away.
//
// One boundary for the whole group: the group component stays mounted across its screens, so only
// the first entry suspends. See `topology.tsx` for the rest of the reasoning, including why the
// trailing `*` is required.

import { Navigate, Route, Routes, useLocation } from 'react-router-dom';
import { TroubleshootCatalogPage } from '../troubleshoot/TroubleshootCatalogPage';
import { RunsPage } from '../troubleshoot/RunsPage';
import { SavedFindingsPage } from '../troubleshoot/SavedFindingsPage';
import { ScheduledPage } from '../troubleshoot/ScheduledPage';
import { ReportRoutePage } from '../troubleshoot/report/ReportRoutePage';

/** Redirect that keeps the query string — a bare `<Navigate to="/x"/>` drops it, which would strip
 *  the `?job=<id>` off existing `/troubleshoot/anomaly?job=…` links and land on an empty report. */
function RedirectPreservingQuery({ to }: { to: string }) {
  const { search } = useLocation();
  return <Navigate to={{ pathname: to, search }} replace />;
}

export default function TroubleshootRoutes() {
  return (
    <Routes>
      <Route index element={<TroubleshootCatalogPage />} />
      <Route path="runs" element={<RunsPage />} />
      {/* One parameterized route serves every tool's report — adding a report touches only the
          registry, never this file. The old per-tool path stays as a redirect for live deep links
          (toasts, bookmarks), preserving `?job=`. */}
      <Route path="report/:tool" element={<ReportRoutePage />} />
      <Route path="anomaly" element={<RedirectPreservingQuery to="/troubleshoot/report/anomaly" />} />
      <Route path="scheduled" element={<ScheduledPage />} />
      <Route path="findings" element={<SavedFindingsPage />} />
      <Route path="*" element={<Navigate to="/dashboard" replace />} />
    </Routes>
  );
}
