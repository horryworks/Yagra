// Persistent UI preferences (Zustand + persist) — layout/theme prefs survive reloads
// (coding-conventions: persistent UI prefs → persisted store; live data stays ephemeral).

import { create } from 'zustand';
import { createJSONStorage, persist, type StateStorage } from 'zustand/middleware';

// localStorage when available (browser), else a no-op — keeps the store working in the Vitest
// node env (no localStorage) without a persist warning. localStorage (not sessionStorage) so UI
// prefs survive across sessions, not just the tab.
const localStore = (): StateStorage =>
  typeof localStorage !== 'undefined'
    ? localStorage
    : { getItem: () => null, setItem: () => undefined, removeItem: () => undefined };

export type Theme = 'light' | 'dark';

/** Interface language. English is the default; others are lazy-loaded (see `i18n.ts`). */
export type Language = 'en' | 'ja';

/** UI layout mode override (ADR-027). `auto` follows the viewport width (mobile < 768px); `desktop`
 *  forces the desktop shell even on a narrow screen. There is no `mobile` value — a wide screen is
 *  always desktop, so the only override worth persisting is "keep desktop". See `lib/viewport.ts`. */
export type UiMode = 'auto' | 'desktop';

/** How the interface Throughput chart's Y-axis treats the configured-bandwidth reference line.
 *  `fit` auto-fits the axis to traffic (line pinned to the top edge until traffic nears it);
 *  `capacity` pins the axis top to the bandwidth so headroom is shown as a proportion. A single
 *  global pref so toggling it on one chart applies everywhere and survives reload. */
export type ThroughputScale = 'fit' | 'capacity';

interface PrefsStore {
  theme: Theme;
  /** Active interface language (default English). Applied app-wide via i18next; see App.tsx. */
  language: Language;
  sidebarCollapsed: boolean;
  /** Inventory-tree groups the user has explicitly collapsed, keyed by group id. The tree
   *  defaults to fully expanded, so we persist the *collapsed* set (empty ⇒ all open) — this
   *  also means a newly-created group, absent from the map, shows expanded automatically. */
  nodeTreeCollapsed: Record<string, true>;
  /** Global Y-axis mode for interface throughput charts (see [`ThroughputScale`]). */
  throughputScale: ThroughputScale;
  /** Layout-mode override; `auto` follows the viewport (see [`UiMode`] / `lib/viewport.ts`). */
  uiMode: UiMode;
  setTheme: (theme: Theme) => void;
  toggleTheme: () => void;
  setLanguage: (language: Language) => void;
  toggleSidebar: () => void;
  /** Flip one inventory-tree group between expanded and collapsed, persisting the choice. */
  toggleNodeTreeGroup: (id: string) => void;
  setThroughputScale: (scale: ThroughputScale) => void;
  /** Flip the throughput Y-axis between fit-to-traffic and scale-to-capacity (global + persisted). */
  toggleThroughputScale: () => void;
  /** Set the layout-mode override (`auto` follows the viewport; `desktop` pins the desktop shell). */
  setUiMode: (mode: UiMode) => void;
}

export const usePrefsStore = create<PrefsStore>()(
  persist(
    (set) => ({
      theme: 'dark',
      language: 'en',
      sidebarCollapsed: false,
      nodeTreeCollapsed: {},
      throughputScale: 'fit',
      uiMode: 'auto',
      setTheme: (theme) => set({ theme }),
      toggleTheme: () => set((s) => ({ theme: s.theme === 'dark' ? 'light' : 'dark' })),
      setLanguage: (language) => set({ language }),
      toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
      toggleNodeTreeGroup: (id) =>
        set((s) => {
          const next = { ...s.nodeTreeCollapsed };
          if (next[id]) delete next[id];
          else next[id] = true;
          return { nodeTreeCollapsed: next };
        }),
      setThroughputScale: (throughputScale) => set({ throughputScale }),
      toggleThroughputScale: () =>
        set((s) => ({ throughputScale: s.throughputScale === 'fit' ? 'capacity' : 'fit' })),
      setUiMode: (uiMode) => set({ uiMode }),
    }),
    { name: 'yagra_prefs', storage: createJSONStorage(localStore) },
  ),
);

/** Reflect the active theme onto <html data-theme> so tokens.css switches palettes. */
export function applyTheme(theme: Theme): void {
  if (typeof document !== 'undefined') {
    document.documentElement.setAttribute('data-theme', theme);
  }
}

/** Reflect the active language onto <html lang> (accessibility / hyphenation / font selection).
 *  The actual string swap is driven by i18next's `changeLanguage`; this just keeps the DOM honest. */
export function applyLanguage(language: Language): void {
  if (typeof document !== 'undefined') {
    document.documentElement.setAttribute('lang', language);
  }
}
