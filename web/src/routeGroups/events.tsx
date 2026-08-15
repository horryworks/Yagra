// SPDX-License-Identifier: AGPL-3.0-only
// Lazy route group: Events — Forwarding, and only Forwarding.
//
// The Events tab has three screens; the other two (the log at `/events`, the webhook list at
// `/events/webhooks`) are imported statically in `routes.tsx`, because the log is on an operator's
// daily path and both already were before ADR-055 moved them here. Forwarding is the tab's one
// configuration-weight screen (774 lines) and was already behind the Settings group's boundary —
// keeping it lazy is what makes this a move rather than a bundle regression.
//
// A group of one, then. That is fine: the reason the split is per-group elsewhere is that a group
// stays mounted so moving between its screens never re-suspends, and there is nothing here to move
// between. See `topology.tsx` for the rest of the reasoning, including why the trailing `*` on the
// parent route is required.

import { Navigate, Route, Routes } from 'react-router-dom';
import { ForwardingPage } from '../pages/ForwardingPage';

export default function EventsRoutes() {
  return (
    <Routes>
      {/* `/events` itself is a static route in `routes.tsx` and outranks the splat that mounts
          this file, so this index only covers the trailing-slash form. */}
      <Route index element={<Navigate to="/events" replace />} />
      <Route path="forwarding" element={<ForwardingPage />} />
      <Route path="*" element={<Navigate to="/events" replace />} />
    </Routes>
  );
}
