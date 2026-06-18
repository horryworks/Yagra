// My Dashboard layout state (Zustand, non-persisted in the browser store — the *server* is the
// source of truth, per-user). Loads on mount; every edit updates local state optimistically and
// schedules a debounced PUT. Unauthenticated (public-dashboard mode) falls back to a read-only
// default and never saves. All mutations delegate to the pure helpers in `layout.ts`.

import { create } from 'zustand';
import { ApiError, api, getToken } from '../services/api';
import {
  addInstance,
  countOfType,
  DASHBOARD_VERSION,
  moveItem,
  removeInstance,
  reorderByIds,
  sanitizeLayout,
  setSettingsById,
  setSpanById,
} from './layout';
import { defaultLayout, getDefinition, registryView } from './registry';
import type { WidgetInstance, WidgetSettings } from './types';

const SAVE_DEBOUNCE_MS = 800;

/** Unique instance id. Browser-only path (crypto.randomUUID); a timestamped fallback covers
 *  older runtimes. The store isn't exercised in the node test env (pure helpers are). */
function makeInstanceId(type: string): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) {
    return `${type}-${crypto.randomUUID()}`;
  }
  return `${type}-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
}

let saveTimer: ReturnType<typeof setTimeout> | undefined;

/** Persist the current widgets after a short quiet period (coalesces rapid edits into one PUT).
 *  Skips persistence when unauthenticated — there's no per-user row to write. */
function scheduleSave(widgets: WidgetInstance[]): void {
  if (!getToken()) return;
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    api
      .putDashboard({ version: DASHBOARD_VERSION, widgets })
      .then(() => useLayoutStore.setState({ saveError: null }))
      .catch((e: unknown) => {
        // Don't swallow the failure: otherwise edits look saved but vanish on the next load.
        // 401 ⇒ the session expired; anything else is treated as transient (retried next edit).
        const msg =
          e instanceof ApiError && e.status === 401
            ? 'Your session expired — sign in again to save dashboard changes.'
            : "Couldn't save your dashboard changes. They'll be retried on your next edit.";
        useLayoutStore.setState({ saveError: msg });
      });
  }, SAVE_DEBOUNCE_MS);
}

type LayoutStatus = 'loading' | 'ready' | 'error';

interface LayoutStore {
  widgets: WidgetInstance[];
  status: LayoutStatus;
  /** A human-readable message when the last persist failed, else null. */
  saveError: string | null;
  /** Customize mode: shows per-widget drag/remove/span controls + the catalog picker. */
  editing: boolean;
  load: () => Promise<void>;
  dismissSaveError: () => void;
  setEditing: (on: boolean) => void;
  addWidget: (type: string) => void;
  removeWidget: (instanceId: string) => void;
  move: (from: number, to: number) => void;
  reorder: (orderedIds: string[]) => void;
  setSpan: (instanceId: string, span: number) => void;
  setSettings: (instanceId: string, patch: WidgetSettings) => void;
  resetToDefault: () => void;
}

export const useLayoutStore = create<LayoutStore>((set, get) => {
  // Apply a new widget list: update state optimistically + schedule the save.
  const apply = (widgets: WidgetInstance[]) => {
    set({ widgets });
    scheduleSave(widgets);
  };

  return {
    widgets: [],
    status: 'loading',
    saveError: null,
    editing: false,

    dismissSaveError: () => set({ saveError: null }),

    load: async () => {
      set({ status: 'loading', saveError: null });
      // Public-dashboard / not logged in: no per-user store — render the default, read-only.
      if (!getToken()) {
        set({ widgets: defaultLayout().widgets, status: 'ready' });
        return;
      }
      try {
        const raw = await api.getDashboard();
        // null ⇒ never saved → default. Otherwise sanitize the saved doc (drops retired widget
        // types, clamps spans). A user who cleared their board keeps an empty (saved) layout.
        const widgets =
          raw == null ? defaultLayout().widgets : sanitizeLayout(raw, registryView).widgets;
        set({ widgets, status: 'ready' });
      } catch {
        set({ widgets: defaultLayout().widgets, status: 'error' });
      }
    },

    setEditing: (on) => set({ editing: on }),

    addWidget: (type) => {
      const def = getDefinition(type);
      if (!def) return;
      const cur = get().widgets;
      if (def.maxInstances != null && countOfType(cur, type) >= def.maxInstances) return;
      apply(addInstance(cur, { instanceId: makeInstanceId(type), type, span: def.defaultSpan }));
    },

    removeWidget: (instanceId) => apply(removeInstance(get().widgets, instanceId)),

    move: (from, to) => apply(moveItem(get().widgets, from, to)),

    reorder: (orderedIds) => apply(reorderByIds(get().widgets, orderedIds)),

    setSpan: (instanceId, span) => apply(setSpanById(get().widgets, instanceId, span, registryView)),

    setSettings: (instanceId, patch) =>
      apply(setSettingsById(get().widgets, instanceId, patch)),

    resetToDefault: () => apply(defaultLayout().widgets),
  };
});
