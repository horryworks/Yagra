// Persistent UI preferences (Zustand + persist) — layout/theme prefs survive reloads
// (coding-conventions: persistent UI prefs → persisted store; live data stays ephemeral).

import { create } from 'zustand';
import { persist } from 'zustand/middleware';

export type Theme = 'light' | 'dark';

interface PrefsStore {
  theme: Theme;
  sidebarCollapsed: boolean;
  setTheme: (theme: Theme) => void;
  toggleTheme: () => void;
  toggleSidebar: () => void;
}

export const usePrefsStore = create<PrefsStore>()(
  persist(
    (set) => ({
      theme: 'dark',
      sidebarCollapsed: false,
      setTheme: (theme) => set({ theme }),
      toggleTheme: () => set((s) => ({ theme: s.theme === 'dark' ? 'light' : 'dark' })),
      toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
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
