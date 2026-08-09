// SPDX-License-Identifier: AGPL-3.0-only
// Lazy route group: Topology (network map, dependency editor, geo map). Mounted by `routes.tsx` at
// `topology/*` through `React.lazy`, so the geo outline (`pages/worldOutline.ts`, ~39 KB of source)
// and the two hand-written SVG views stay out of the initial chunk — an operator who only opens the
// dashboard never downloads them (ADR-027 makes mobile a supported persona).
//
// The whole group is ONE boundary on purpose: the group component stays mounted while the operator
// moves between its screens, so only the first entry suspends. Splitting per page would re-suspend
// on every tab switch and flash the fallback.
//
// The trailing `*` mirrors the fallback in `routes.tsx`: without it an unknown `/topology/…` path
// would be captured by this group and render nothing instead of redirecting.

import { Navigate, Route, Routes } from 'react-router-dom';
import { TopologyMapPage } from '../pages/TopologyMapPage';
import { DependencyPage } from '../pages/DependencyPage';
import { GeoMapPage } from '../pages/GeoMapPage';

export default function TopologyRoutes() {
  return (
    <Routes>
      <Route path="map" element={<TopologyMapPage />} />
      <Route path="dependency" element={<DependencyPage />} />
      <Route path="geo" element={<GeoMapPage />} />
      <Route path="*" element={<Navigate to="/dashboard" replace />} />
    </Routes>
  );
}
