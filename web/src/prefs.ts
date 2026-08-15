// SPDX-License-Identifier: AGPL-3.0-only
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
  /** Collapse the Nodes page inventory-tree pane to a slim rail so the node detail uses the full
   *  width (desktop only; on mobile the pane switcher governs). */
  nodesPaneCollapsed: boolean;
  /** Whether the column filter row is open on desktop (ADR-053 Inc.9). **Closed by default** — the
   *  row reached every list in Inc.0–8 and then occupied a band on screens nobody was filtering.
   *
   *  One global boolean rather than a per-screen map, and the reason it *can* be one is
   *  `lib/filterRow.ts`: a list that is actually being narrowed shows its row regardless, so there
   *  is no "closed but filtering" state anyone has to remember. A screen with several tables
   *  (Reports has three) therefore opens and closes them together, which is the accepted cost. */
  filterRowOpen: boolean;
  /** How tall the operator dragged the node-detail Interfaces chart dock, in px (issue #65).
   *  `null` = never resized, so the dock picks a height from the container it is actually in
   *  instead of pinning whatever one window happened to be on the day it was first opened.
   *
   *  ⚠️ **This one has a second, authoritative home: the server** (ADR-058, `serverPrefs.ts`). The
   *  copy here is the local cache — it paints the first frame with no flash, and it is the whole
   *  answer when signed out, offline, or talking to a core that predates the endpoint. The sync
   *  module adopts the server's value into this field on load and mirrors writes back out; nothing
   *  else should call the setter directly. */
  interfaceDockHeight: number | null;
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
  /** Toggle the Nodes inventory pane between full and a slim rail (persisted). */
  toggleNodesPane: () => void;
  /** Show or hide the desktop column filter row (global + persisted; see [`filterRowOpen`]). */
  toggleFilterRow: () => void;
  /** Record the Interfaces dock height locally. ⚠️ Prefer `serverPrefs.ts`'s setter, which also
   *  syncs it to the account (see [`interfaceDockHeight`]). */
  setInterfaceDockHeight: (px: number | null) => void;
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
      nodesPaneCollapsed: false,
      // Absent from every `yagra_prefs` written before this shipped. `persist` merges the stored
      // object over the initial state, so a missing key reads as `false` and no migration is owed.
      filterRowOpen: false,
      // Absent from every `yagra_prefs` written before this shipped; `persist` merges the stored
      // object over the initial state, so a missing key reads as `null` and no migration is owed.
      interfaceDockHeight: null,
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
      toggleNodesPane: () => set((s) => ({ nodesPaneCollapsed: !s.nodesPaneCollapsed })),
      toggleFilterRow: () => set((s) => ({ filterRowOpen: !s.filterRowOpen })),
      setInterfaceDockHeight: (interfaceDockHeight) => set({ interfaceDockHeight }),
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
