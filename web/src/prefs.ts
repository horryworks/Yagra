// Persistent UI preferences (Zustand + persist) — layout/theme prefs survive reloads
// (coding-conventions: persistent UI prefs → persisted store; live data stays ephemeral).

import { create } from 'zustand';
import { persist } from 'zustand/middleware';

export type Theme = 'light' | 'dark';

interface PrefsStore {
  theme: Theme;
  sidebarCollapsed: boolean;
  /** Inventory-tree groups the user has explicitly collapsed, keyed by group id. The tree
   *  defaults to fully expanded, so we persist the *collapsed* set (empty ⇒ all open) — this
   *  also means a newly-created group, absent from the map, shows expanded automatically. */
  nodeTreeCollapsed: Record<string, true>;
  setTheme: (theme: Theme) => void;
  toggleTheme: () => void;
  toggleSidebar: () => void;
  /** Flip one inventory-tree group between expanded and collapsed, persisting the choice. */
  toggleNodeTreeGroup: (id: string) => void;
}

export const usePrefsStore = create<PrefsStore>()(
  persist(
    (set) => ({
      theme: 'dark',
      sidebarCollapsed: false,
      nodeTreeCollapsed: {},
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
    }),
    { name: 'yagra_prefs' },
  ),
);

/** Reflect the active theme onto <html data-theme> so tokens.css switches palettes. */
export function applyTheme(theme: Theme): void {
  if (typeof document !== 'undefined') {
    document.documentElement.setAttribute('data-theme', theme);
  }
}
