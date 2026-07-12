// Canonical responsive layout boundary + viewport-mode plumbing (ADR-027, ui-conventions.md
// "Responsive / mobile"). ONE boundary (768px), exposed two ways:
//   1. React hooks (useSyncExternalStore over matchMedia) for JS-side decisions.
//   2. a `<html data-viewport="mobile|desktop">` stamp CSS keys mobile layout off of.
// Mirrors applyTheme/applyLanguage in prefs.ts. The `uiMode` pref can force desktop on a narrow
// screen; there is intentionally no "force mobile" (a wide screen is always desktop).

import { useSyncExternalStore } from 'react';
import { usePrefsStore, type UiMode } from '../prefs';

/** The one canonical layout boundary. A viewport narrower than this (`max-width: 767px`) is mobile
 *  mode. This is the single TS source of truth; the CSS side documents the same 768 constant because
 *  a CSS custom property can't be used inside an `@media` condition (tokens.css / ui-conventions.md). */
export const MOBILE_BP = 768;

const mobileQuery = `(max-width: ${MOBILE_BP - 1}px)`;
const coarseQuery = '(hover: none)';

/** A tiny matchMedia → useSyncExternalStore adapter (one shared MediaQueryList per query). Degrades
 *  to a constant `false` when there is no `window`/`matchMedia` (SSR, the Vitest node env). */
function mediaStore(query: string) {
  const mql =
    typeof window !== 'undefined' && typeof window.matchMedia === 'function'
      ? window.matchMedia(query)
      : null;
  return {
    subscribe(onChange: () => void): () => void {
      if (!mql) return () => {};
      // Safari < 14 only has addListener/removeListener; support both.
      if (typeof mql.addEventListener === 'function') {
        mql.addEventListener('change', onChange);
        return () => mql.removeEventListener('change', onChange);
      }
      mql.addListener(onChange);
      return () => mql.removeListener(onChange);
    },
    get: (): boolean => (mql ? mql.matches : false),
  };
}

const mobile = mediaStore(mobileQuery);
const coarse = mediaStore(coarseQuery);

/** True when the viewport is narrower than the mobile boundary. `false` without matchMedia. */
export function useIsMobileViewport(): boolean {
  return useSyncExternalStore(mobile.subscribe, mobile.get, () => false);
}

/** True on a coarse (touch) pointer — for touch-affordance decisions that are independent of the
 *  layout mode (a touch laptop in forced-desktop view still wants touch-reachable controls). */
export function useIsCoarsePointer(): boolean {
  return useSyncExternalStore(coarse.subscribe, coarse.get, () => false);
}

/** The pure layout-mode decision: `desktop` pref forces desktop; `auto` follows the viewport. */
export function resolveViewportMode(uiMode: UiMode, narrow: boolean): 'mobile' | 'desktop' {
  if (uiMode === 'desktop') return 'desktop';
  return narrow ? 'mobile' : 'desktop';
}

/** The effective layout mode, combining the live viewport width with the persisted `uiMode`. */
export function useViewportMode(): 'mobile' | 'desktop' {
  const narrow = useIsMobileViewport();
  const uiMode = usePrefsStore((s) => s.uiMode);
  return resolveViewportMode(uiMode, narrow);
}

/** Reflect the resolved mode onto `<html data-viewport>` so mode-dependent CSS
 *  (`html[data-viewport='mobile'] …`) applies. Mirrors applyTheme(); a no-op without a document. */
export function applyViewportMode(mode: 'mobile' | 'desktop'): void {
  if (typeof document !== 'undefined') {
    document.documentElement.setAttribute('data-viewport', mode);
  }
}
