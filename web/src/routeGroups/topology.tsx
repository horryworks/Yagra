// SPDX-License-Identifier: AGPL-3.0-only
// Lazy route group: Topology (network map, geo map). Mounted by `routes.tsx` at
// `topology/*` through `React.lazy`, so the geo outline (`pages/worldOutline.ts`, ~620 KB of
// source, ~170 KB over the wire) and the two hand-written SVG views stay out of the initial chunk —
// an operator who only opens the dashboard never downloads them (ADR-027 makes mobile a supported
// persona). That outline is the reason this group is lazy at all: it grew sixteenfold when the
// coastline moved from the 1:110m set to 1:10m, and it is the largest single constant in the app.
//
// The whole group is ONE boundary on purpose: the group component stays mounted while the operator
// moves between its screens, so only the first entry suspends. Splitting per page would re-suspend
// on every tab switch and flash the fallback.
//
// The trailing `*` mirrors the fallback in `routes.tsx`: without it an unknown `/topology/…` path
// would be captured by this group and render nothing instead of redirecting.

import { Navigate, Route, Routes } from 'react-router-dom';
import { TopologyMapPage } from '../pages/TopologyMapPage';
import { MovedTo } from '../components/shell/MovedTo';
import { GeoMapPage } from '../pages/GeoMapPage';

export default function TopologyRoutes() {
  return (
    <Routes>
      <Route path="map" element={<TopologyMapPage />} />
      {/* The dependency screen is gone (ADR-236); how alerts are grouped is set on Routing. */}
      <Route path="dependency" element={<MovedTo to="/alerts/routing" />} />
      <Route path="geo" element={<GeoMapPage />} />
      <Route path="*" element={<Navigate to="/dashboard" replace />} />
    </Routes>
  );
}
