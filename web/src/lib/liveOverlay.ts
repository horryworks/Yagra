// SPDX-License-Identifier: AGPL-3.0-only
// Overlaying the live node-state map (S14, `dashboard/useNodeStates`) onto a fetched list, without
// producing a fresh array on every SSE flush.
//
// `useNodeStates` publishes a NEW Map identity whenever ANY node in the fleet changes state — a
// view that overlays it with a bare `base.map(...)` therefore hands its consumers a new array even
// when not one of ITS rows differs. Downstream that array is a `useMemo` key for genuinely
// expensive work (the topology graph layout; `buildNodeTree` + `flattenTree` for the inventory), so
// an unrelated node flipping state anywhere in the fleet re-does all of it. During a post-restart
// first-observe burst the flush coalescer fires ~10×/s, which is exactly when an operator is
// staring at the screen.
//
// So this returns a *stable identity* when the visible result is unchanged:
//   - nothing overridden      → `base` itself (already stable, from its own memo)
//   - overridden, same as last→ the previous output array
//   - anything actually moved → a fresh array
// The caller keeps the returned record in a ref and passes it back in. Calling twice with the same
// inputs returns the same record, so it is safe under StrictMode's double render.

import type { NodeState } from '../types/api';

/** The shape this works on: anything carrying a node id and the state to overlay. */
export interface LiveStated {
  id: string;
  state: NodeState;
}

/** The previous result plus the base it was derived from — both are needed to know it still holds:
 *  the non-state fields of `out` come from `base`, so a new `base` invalidates it outright. */
export interface LiveOverlay<T extends LiveStated> {
  base: T[];
  out: T[];
}

/** True when `out` still shows exactly what `base` overlaid with `live` would show. Only valid when
 *  `out` was derived from this same `base` (checked by the caller in `overlayLiveStates`). */
function stillCurrent<T extends LiveStated>(
  out: T[],
  base: T[],
  live: ReadonlyMap<string, NodeState>,
): boolean {
  for (let i = 0; i < base.length; i++) {
    if (out[i].state !== (live.get(base[i].id) ?? base[i].state)) return false;
  }
  return true;
}

/**
 * Overlay `live` onto `base`, reusing the previous array when the visible result did not change.
 *
 * The returned record is what the caller stores for the next call; read `.out` for the list.
 */
export function overlayLiveStates<T extends LiveStated>(
  base: T[],
  live: ReadonlyMap<string, NodeState>,
  prev: LiveOverlay<T> | null,
): LiveOverlay<T> {
  if (prev && prev.base === base && stillCurrent(prev.out, base, live)) return prev;

  let overridden = false;
  const out = base.map((n) => {
    const s = live.get(n.id);
    if (s !== undefined && s !== n.state) {
      overridden = true;
      return { ...n, state: s };
    }
    return n;
  });
  // Nothing was overridden ⇒ the overlay is `base` element for element. Hand back `base` itself so
  // the identity is the caller's own (already memoized) one rather than a per-flush copy.
  return { base, out: overridden ? out : base };
}
