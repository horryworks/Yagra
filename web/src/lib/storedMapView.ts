// SPDX-License-Identifier: AGPL-3.0-only
// A `useState`-shaped binding to the session's remembered map view (ADR-134).
//
// Both maps hold their pan/zoom as `const [view, setView] = useState<View | null>(null)` and both
// lean on exactly two properties of that: the setter accepts an updater function (every wheel and
// pinch handler reads the live value), and `null` means "never positioned", which is what lets the
// first measured frame auto-fit without the 15s refresh stomping the operator afterwards.
//
// So this is deliberately shaped like the `useState` it replaces, and deliberately holds no
// judgement: resolving an updater against the stored value happens in the store (`setMapView`),
// where a test can run it. What is left here is the binding, which is what `.tsx` files may hold.
//
// ⚠️ One memory per map. `TopologyMap`'s `View` and `GeoMapPage`'s `GeoView` are the same three
// numbers but they are transforms over different things (a laid-out diagram vs. a world
// projection), so sharing one slot would restore a world-scale pan onto a topology.

import { useCallback } from 'react';
import { useMapViewStore, type MapView, type MapViewKey } from '../store';

/** The stored view for one map plus a `useState`-compatible setter. */
export function useStoredMapView(
  key: MapViewKey,
): [MapView | null, (next: MapView | null | ((prev: MapView | null) => MapView | null)) => void] {
  const view = useMapViewStore((s) => s[key]);
  const setMapView = useMapViewStore((s) => s.setMapView);
  // Stable across renders: both maps pass their setter into `useCallback` dependency lists, and an
  // identity that changed every render would rebuild every gesture handler on every refresh tick.
  const setView = useCallback(
    (next: MapView | null | ((prev: MapView | null) => MapView | null)) => setMapView(key, next),
    [key, setMapView],
  );
  return [view, setView];
}
