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

/** How the interface Throughput chart's Y-axis treats the configured-bandwidth reference line.
 *  `fit` auto-fits the axis to traffic (line pinned to the top edge until traffic nears it);
 *  `capacity` pins the axis top to the bandwidth so headroom is shown as a proportion. A single
 *  global pref so toggling it on one chart applies everywhere and survives reload. */
export type ThroughputScale = 'fit' | 'capacity';

interface PrefsStore {
  theme: Theme;
  sidebarCollapsed: boolean;
  /** Inventory-tree groups the user has explicitly collapsed, keyed by group id. The tree
   *  defaults to fully expanded, so we persist the *collapsed* set (empty ⇒ all open) — this
   *  also means a newly-created group, absent from the map, shows expanded automatically. */
  nodeTreeCollapsed: Record<string, true>;
  /** Global Y-axis mode for interface throughput charts (see [`ThroughputScale`]). */
  throughputScale: ThroughputScale;
  setTheme: (theme: Theme) => void;
  toggleTheme: () => void;
  toggleSidebar: () => void;
  /** Flip one inventory-tree group between expanded and collapsed, persisting the choice. */
  toggleNodeTreeGroup: (id: string) => void;
  setThroughputScale: (scale: ThroughputScale) => void;
  /** Flip the throughput Y-axis between fit-to-traffic and scale-to-capacity (global + persisted). */
  toggleThroughputScale: () => void;
}

export const usePrefsStore = create<PrefsStore>()(
  persist(
    (set) => ({
      theme: 'dark',
      sidebarCollapsed: false,
      nodeTreeCollapsed: {},
      throughputScale: 'fit',
      setTheme: (theme) => set({ theme }),
      toggleTheme: () => set((s) => ({ theme: s.theme === 'dark' ? 'light' : 'dark' })),
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
