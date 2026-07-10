// Dashboard layout state (Zustand, non-persisted in the browser store — the *server* is the
// source of truth). `createLayoutStore` builds an independent store from a load/save pair, so the
// per-user "My Dashboard" (useLayoutStore) and the global "Shared Dashboard" (useSharedLayoutStore)
// each get their own state + debounce timer without sharing one. Loads on mount; every edit updates
// local state optimistically and schedules a debounced save. Unauthenticated (public-dashboard mode)
// falls back to a read-only default and never saves. All mutations delegate to the pure helpers in
// `layout.ts`.
//
// Documents are multi-board (schema v2): the store keeps `boards` + an *ephemeral* `activeBoardId`,
// and exposes `widgets` = the active board's widgets so the grid/WidgetFrame/CatalogModal consume an
// unchanged shape. Widget mutations target the active board; board actions add/remove/rename/switch.

import { create } from 'zustand';
import i18n from '../i18n';
import { ApiError, api, getToken } from '../services/api';
import {
  addBoard,
  addInstance,
  countOfType,
  DASHBOARD_VERSION,
  moveItem,
  removeBoard,
  removeInstance,
  renameBoard,
  reorderByIds,
  sanitizeLayout,
  setBoardWidgets,
  setSettingsById,
  setSpanById,
} from './layout';
import { defaultLayout, getDefinition, registryView } from './registry';
import type { Board, DashboardLayout, WidgetInstance, WidgetSettings } from './types';

const SAVE_DEBOUNCE_MS = 800;

/** Unique widget instance id. Browser-only path (crypto.randomUUID); a timestamped fallback covers
 *  older runtimes. The store isn't exercised in the node test env (pure helpers are). */
function makeInstanceId(type: string): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) {
    return `${type}-${crypto.randomUUID()}`;
  }
  return `${type}-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
}

/** Unique board id (same strategy as instance ids). */
function makeBoardId(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) {
    return `board-${crypto.randomUUID()}`;
  }
  return `board-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
}

type LayoutStatus = 'loading' | 'ready' | 'error';

export interface LayoutStore {
  /** All boards (persisted). */
  boards: Board[];
  /** The board currently shown — ephemeral UI selection, not persisted in the document. */
  activeBoardId: string;
  /** The active board's widgets (derived; kept in sync so consumers stay shape-stable). */
  widgets: WidgetInstance[];
  status: LayoutStatus;
  /** A human-readable message when the last persist failed, else null. */
  saveError: string | null;
  /** Customize mode: shows per-widget drag/remove/span controls + the catalog picker. */
  editing: boolean;
  load: () => Promise<void>;
  dismissSaveError: () => void;
  setEditing: (on: boolean) => void;
  // Widget actions — operate on the active board.
  addWidget: (type: string) => void;
  removeWidget: (instanceId: string) => void;
  move: (from: number, to: number) => void;
  reorder: (orderedIds: string[]) => void;
  setSpan: (instanceId: string, span: number) => void;
  setSettings: (instanceId: string, patch: WidgetSettings) => void;
  resetToDefault: () => void;
  // Board actions.
  setActiveBoard: (id: string) => void;
  addBoard: (name?: string) => void;
  removeBoard: (id: string) => void;
  renameBoard: (id: string, name: string) => void;
}

/** What a store needs to load/save its document and seed an empty/unauthenticated default. */
export interface LayoutStoreConfig {
  load: () => Promise<unknown>;
  save: (doc: DashboardLayout) => Promise<unknown>;
  defaultDoc: () => DashboardLayout;
}

/** Build an independent layout store. Each instance owns its own debounce timer (declared in the
 *  closure) so two boards never clobber each other's saves. */
export function createLayoutStore(config: LayoutStoreConfig) {
  let saveTimer: ReturnType<typeof setTimeout> | undefined;

  return create<LayoutStore>((set, get) => {
    /** Persist the current boards after a short quiet period (coalesces rapid edits into one save).
     *  Skips when unauthenticated — there's no row to write. */
    const scheduleSave = (boards: Board[]) => {
      if (!getToken()) return;
      if (saveTimer) clearTimeout(saveTimer);
      saveTimer = setTimeout(() => {
        config
          .save({ version: DASHBOARD_VERSION, boards })
          .then(() => set({ saveError: null }))
          .catch((e: unknown) => {
            // Don't swallow the failure: otherwise edits look saved but vanish on the next load.
            // 401 ⇒ session expired; 403 ⇒ not permitted (shared board, non-admin); else transient.
            // Resolve via the global i18n instance (like lib/format) so the message follows the
            // active language without threading a hook through this store.
            const msg =
              e instanceof ApiError && e.status === 401
                ? i18n.t('dashboard:save.expired')
                : e instanceof ApiError && e.status === 403
                  ? i18n.t('dashboard:save.forbidden')
                  : i18n.t('dashboard:save.failed');
            set({ saveError: msg });
          });
      }, SAVE_DEBOUNCE_MS);
    };

    /** The active board's widgets (empty if the id is stale). */
    const widgetsOf = (boards: Board[], activeBoardId: string): WidgetInstance[] =>
      boards.find((b) => b.id === activeBoardId)?.widgets ?? [];

    /** Commit a new boards array: update state (+ derived widgets) optimistically and save. */
    const commit = (boards: Board[], activeBoardId = get().activeBoardId) => {
      set({ boards, activeBoardId, widgets: widgetsOf(boards, activeBoardId) });
      scheduleSave(boards);
    };

    /** Apply a new widget list to the active board. */
    const applyWidgets = (widgets: WidgetInstance[]) => {
      const { boards, activeBoardId } = get();
      commit(setBoardWidgets(boards, activeBoardId, widgets));
    };

    /** Adopt a full document (load): show the first board, no save. */
    const adopt = (doc: DashboardLayout, status: LayoutStatus) => {
      const boards = doc.boards.length ? doc.boards : config.defaultDoc().boards;
      const activeBoardId = boards[0].id;
      set({ boards, activeBoardId, widgets: widgetsOf(boards, activeBoardId), status });
    };

    return {
      boards: [],
      activeBoardId: '',
      widgets: [],
      status: 'loading',
      saveError: null,
      editing: false,

      dismissSaveError: () => set({ saveError: null }),

      load: async () => {
        set({ status: 'loading', saveError: null });
        // Public-dashboard / not logged in: no row to read — render the default, read-only.
        if (!getToken()) {
          adopt(config.defaultDoc(), 'ready');
          return;
        }
        try {
          const raw = await config.load();
          // null ⇒ never saved → default. Otherwise sanitize the saved doc (migrates v1, drops
          // retired widget types, clamps spans). A cleared board stays cleared.
          const doc = raw == null ? config.defaultDoc() : sanitizeLayout(raw, registryView);
          adopt(doc, 'ready');
        } catch {
          adopt(config.defaultDoc(), 'error');
        }
      },

      setEditing: (on) => set({ editing: on }),

      addWidget: (type) => {
        const def = getDefinition(type);
        if (!def) return;
        const cur = get().widgets;
        if (def.maxInstances != null && countOfType(cur, type) >= def.maxInstances) return;
        applyWidgets(
          addInstance(cur, { instanceId: makeInstanceId(type), type, span: def.defaultSpan }),
        );
      },

      removeWidget: (instanceId) => applyWidgets(removeInstance(get().widgets, instanceId)),

      move: (from, to) => applyWidgets(moveItem(get().widgets, from, to)),

      reorder: (orderedIds) => applyWidgets(reorderByIds(get().widgets, orderedIds)),

      setSpan: (instanceId, span) =>
        applyWidgets(setSpanById(get().widgets, instanceId, span, registryView)),

      setSettings: (instanceId, patch) =>
        applyWidgets(setSettingsById(get().widgets, instanceId, patch)),

      // Reset just the *active* board to the default widget set.
      resetToDefault: () =>
        applyWidgets(config.defaultDoc().boards[0].widgets.map((w) => ({ ...w }))),

      setActiveBoard: (id) => {
        const { boards } = get();
        if (!boards.some((b) => b.id === id)) return;
        set({ activeBoardId: id, widgets: widgetsOf(boards, id) });
      },

      addBoard: (name) => {
        const { boards } = get();
        const id = makeBoardId();
        const boardName = name?.trim() || `Dashboard ${boards.length + 1}`;
        commit(addBoard(boards, { id, name: boardName, widgets: [] }), id); // new board becomes active
      },

      removeBoard: (id) => {
        const { boards, activeBoardId } = get();
        const next = removeBoard(boards, id);
        if (next === boards) return; // guarded (last board) or unknown id
        const nextActive = activeBoardId === id ? next[0].id : activeBoardId;
        commit(next, nextActive);
      },

      renameBoard: (id, name) => commit(renameBoard(get().boards, id, name)),
    };
  });
}

/** The type of a layout store hook (used by the store-injection context). */
export type LayoutStoreApi = ReturnType<typeof createLayoutStore>;

/** My Dashboard — per-user layout. */
export const useLayoutStore = createLayoutStore({
  load: () => api.getDashboard(),
  save: (doc) => api.putDashboard(doc),
  defaultDoc: defaultLayout,
});

/** Shared Dashboard — one global layout shown to all users. Reads are open; saves are admin-only
 *  (a non-admin save 403s, surfaced as a saveError). The UI gates the customize control to admins. */
export const useSharedLayoutStore = createLayoutStore({
  load: () => api.getSharedDashboard(),
  save: (doc) => api.putSharedDashboard(doc),
  defaultDoc: defaultLayout,
});
