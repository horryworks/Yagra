// SPDX-License-Identifier: AGPL-3.0-only
// `/troubleshoot/report/:tool` — resolves the tool slug to its descriptor and hands it to the shell.
//
// One parameterized route rather than 15 static ones: later increments add a registry entry and a
// body file without touching routes.tsx at all, and a single future `React.lazy` boundary here would
// cover every report.

import { Navigate, useParams } from 'react-router-dom';
import { REPORTS } from './registry';
import { ReportShell } from './ReportShell';
import type { AnalysisToolKey } from '../../types/api';

export function ReportRoutePage() {
  const { tool } = useParams<{ tool: string }>();
  const descriptor = tool ? REPORTS[tool as AnalysisToolKey] : undefined;
  // Unknown slug, or a tool whose report isn't built yet — back to the catalog rather than a blank
  // screen. (`data.ts` only advertises a `reportPath` for tools that ARE in the registry, so a
  // "View →" button never lands here.)
  if (!descriptor) return <Navigate to="/troubleshoot" replace />;
  return <ReportShell descriptor={descriptor} />;
}
