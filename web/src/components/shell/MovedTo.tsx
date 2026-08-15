// SPDX-License-Identifier: AGPL-3.0-only
// A redirect for a screen that changed address, keeping the query string (ADR-055 Inc.2).
//
// ⚠️ `<Navigate to="/events" replace />` — the obvious spelling, and the one the older
// `/nodes/dependencies` redirect uses — DROPS the search params. That is harmless for a screen
// nothing links to with a query, and wrong for `/alerts/events?node_id=<uuid>`, which is what the
// node-detail Events tab has always linked to and what an operator pastes into a chat during an
// incident. Landing on every node's events instead of one node's is not an error anyone sees: the
// page renders perfectly, showing the wrong set. So the failure is silent, which is why this
// exists rather than four hand-written `<Navigate>`s that each look fine.
//
// The hash is carried too. It costs nothing and a dropped anchor is the same class of quiet loss.

import { Navigate, useLocation } from 'react-router-dom';

/** Redirect to `to`, carrying whatever query string and hash the old URL arrived with. */
export function MovedTo({ to }: { to: string }) {
  const { search, hash } = useLocation();
  return <Navigate to={`${to}${search}${hash}`} replace />;
}
