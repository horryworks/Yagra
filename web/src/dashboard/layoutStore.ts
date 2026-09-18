// SPDX-License-Identifier: AGPL-3.0-only
// Dashboard layout state (Zustand, non-persisted in the browser store — the *server* is the
// source of truth). `createLayoutStore` builds an independent store from a load/save pair, so the
// per-user "My Dashboard" (useLayoutStore) and the global "Shared Dashboard" (useSharedLayoutStore)
// each get their own state + debounce timer without sharing one. Loads on mount; every edit updates
// local state optimistically and schedules a debounced save. Unauthenticated (public-dashboard mode)
// falls back to a read-only default and never saves. All mutations delegate to the pure helpers in
// `layout.ts`.
//
// Documents are multi-board (schema v2): the store keeps `boards` + `activeBoardId`, and exposes
// `widgets` = the active board's widgets so the grid/WidgetFrame/CatalogModal consume an unchanged
// shape. Widget mutations target the active board; board actions add/remove/rename/switch.
//
// `activeBoardId` is **not part of the saved document** — which board you are looking at is yours,
// not something other sessions and other machines vote on. It is not ephemeral either: since
// ADR-134 the session remembers it (`useLastBoardStore`), because `load()` runs on every mount and
// returning to the dashboard therefore put a multi-board operator back on board 1 every time.

import { create } from 'zustand';
import i18n from '../i18n';
import { ApiError, api } from '../services/api';
import { currentViewer, useLastBoardStore } from '../store';
import { mayLoad, maySave, type BoardGate } from './layoutAccess';
import {
  addBoard,
  addInstance,
  countOfType,
  DASHBOARD_VERSION,
  moveItem,
  removeBoard,
  removeInstance,
  renameBoard,
  renameWidgetById,
  reorderByIds,
  sanitizeLayout,
  setBoardWidgets,
  setSettingsById,
  setSizeById,
} from './layout';
import { defaultLayout, emptyPublicLayout, getDefinition, registryView } from './registry';
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
  /** The board currently shown. Never written to the saved document; restored for the session from
   *  `useLastBoardStore` (ADR-134). Every write of it goes through `showBoard`. */
  activeBoardId: string;
  /** The active board's widgets (derived; kept in sync so consumers stay shape-stable). */
  widgets: WidgetInstance[];
  status: LayoutStatus;
  /** Whether this session has ever held the document the server holds. **Nothing is saved until
   *  it has**, and the pages draw no edit control until it has.
   *
   *  🚨 A load that failed used to adopt the five-widget default as if it were the operator's own
   *  board, and the first edit after that PUT the default over every board they had built. A failed
   *  read is not an empty board, and an empty board must never be written back. Separate from
   *  `status` on purpose: a later refresh that fails leaves `loaded` true, because what is on
   *  screen is still the real document. */
  loaded: boolean;
  /** A human-readable message when the last persist failed, else null. */
  saveError: string | null;
  /** Customize mode: shows per-widget drag/remove/resize controls + the catalog picker. */
  editing: boolean;
  load: () => Promise<void>;
  dismissSaveError: () => void;
  /** Enter/leave edit mode. Entering snapshots the current boards so {@link cancelEditing} can
   *  revert; leaving with `false` (Done) keeps the live-saved changes. */
  setEditing: (on: boolean) => void;
  /** Discard everything changed since entering edit mode: restore the entry snapshot (and persist
   *  it, undoing the intermediate debounced saves) and leave edit mode. */
  cancelEditing: () => void;
  /** True when the board differs from the edit-entry snapshot (drives the Cancel confirm). */
  isDirty: () => boolean;
  // Widget actions — operate on the active board.
  addWidget: (type: string) => void;
  removeWidget: (instanceId: string) => void;
  move: (from: number, to: number) => void;
  reorder: (orderedIds: string[]) => void;
  setSize: (instanceId: string, span: number, rowSpan: number) => void;
  setSettings: (instanceId: string, patch: WidgetSettings) => void;
  /** Give one placed widget its own name; a blank one clears it (ADR-071). */
  renameWidget: (instanceId: string, title: string) => void;
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
  /** Which credential this board's `GET` needs — the guard its API route takes.
   *
   *  Required, because the three boards genuinely differ and the old code assumed they did not: it
   *  skipped the fetch whenever there was no token, which is right for My Dashboard and wrong for
   *  the other two. See `layoutAccess.ts`. */
  readGate: BoardGate;
  /** Which dashboard this is, for the session's "last board shown" memory (ADR-134).
   *
   *  Its own key per store, not one shared one: the three are separate documents with separate
   *  board sets, so a shared key would name a board the other two have never heard of. */
  key: 'my' | 'shared' | 'public';
}

/** Build an independent layout store. Each instance owns its own debounce timer (declared in the
 *  closure) so two boards never clobber each other's saves. */
/** Full deep copy of a boards array (widgets + settings), so the edit snapshot can't share mutable
 *  references with live state. The layout is small and JSON-serializable (it's persisted as JSON),
 *  so a JSON round-trip is the simplest guaranteed-deep clone. */
function cloneBoards(boards: Board[]): Board[] {
  return JSON.parse(JSON.stringify(boards)) as Board[];
}

export function createLayoutStore(config: LayoutStoreConfig) {
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  // What the armed timer will write, so `load()` can send it NOW instead of racing it.
  let pendingSave: Board[] | null = null;
  // Bumped by every edit. A load that was already in flight when one happened is answering a
  // question that is now out of date, and adopting it would silently undo the edit on screen.
  let editSeq = 0;
  // Snapshot of `boards` taken when edit mode is entered; null outside an edit session. Cancel
  // restores it. Kept in the closure (like saveTimer) rather than store state — it's edit-session
  // scratch, not something the UI subscribes to.
  let editSnapshot: Board[] | null = null;

  return create<LayoutStore>((set, get) => {
    /** Persist the current boards after a short quiet period (coalesces rapid edits into one save).
     *  Skips when unauthenticated — there's no row to write. */
    /** Write `boards` now. Never rejects — a failure becomes `saveError`. */
    const persist = (boards: Board[]): Promise<void> =>
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

    /** Send the armed save immediately, if there is one. */
    const flushSave = (): Promise<void> => {
      if (saveTimer) clearTimeout(saveTimer);
      saveTimer = undefined;
      const boards = pendingSave;
      pendingSave = null;
      return boards ? persist(boards) : Promise.resolve();
    };

    const scheduleSave = (boards: Board[]) => {
      // 🚨 Session only, deliberately — `maySave` takes no permission. Gating this on
      // `manage_config` would discard an admin's edit during the window before the role matrix
      // arrives, because `useCan` is fail-closed while it resolves. Which boards a signed-in user
      // may actually write is enforced by the API guard and by not drawing the Customize button.
      if (!maySave(currentViewer())) return;
      // See `loaded`. The pages do not draw an edit control before a load has succeeded, so
      // this is the backstop — and it says so, because an edit that is quietly not saved is the
      // other half of the same defect.
      if (!get().loaded) {
        set({ saveError: i18n.t('dashboard:save.notLoaded') });
        return;
      }
      if (saveTimer) clearTimeout(saveTimer);
      pendingSave = boards;
      saveTimer = setTimeout(() => void flushSave(), SAVE_DEBOUNCE_MS);
    };

    /** The active board's widgets (empty if the id is stale). */
    const widgetsOf = (boards: Board[], activeBoardId: string): WidgetInstance[] =>
      boards.find((b) => b.id === activeBoardId)?.widgets ?? [];

    /** Show a board, and remember it for the next visit (ADR-134). One funnel for every write of
     *  `activeBoardId`, so a path that switches boards cannot forget to record it — `removeBoard`
     *  and `cancelEditing` both reach it through `commit`. */
    const showBoard = (patch: {
      boards: Board[];
      activeBoardId: string;
      status?: LayoutStatus;
    }) => {
      useLastBoardStore.getState().rememberBoard(config.key, patch.activeBoardId);
      set({ ...patch, widgets: widgetsOf(patch.boards, patch.activeBoardId) });
    };

    /** Commit a new boards array: update state (+ derived widgets) optimistically and save. */
    const commit = (boards: Board[], activeBoardId = get().activeBoardId) => {
      editSeq += 1;
      showBoard({ boards, activeBoardId });
      scheduleSave(boards);
    };

    /** Apply a new widget list to the active board. */
    const applyWidgets = (widgets: WidgetInstance[]) => {
      const { boards, activeBoardId } = get();
      commit(setBoardWidgets(boards, activeBoardId, widgets));
    };

    /** Adopt a full document (load): show the board this session was last on, no save.
     *
     *  ⚠️ **`load()` runs on every mount**, so this line is what decided the board on every return
     *  to the dashboard — and it was `boards[0].id`, which put a multi-board operator back on board
     *  1 every single time (ADR-134). The remembered id is checked against the document rather than
     *  trusted: a board renamed elsewhere keeps its id, but a *removed* one must fall back rather
     *  than leave the grid showing the empty widget list of a board that is gone. */
    const adopt = (doc: DashboardLayout, status: LayoutStatus) => {
      const boards = doc.boards.length ? doc.boards : config.defaultDoc().boards;
      const remembered = useLastBoardStore.getState().byBoard[config.key];
      const activeBoardId = boards.some((b) => b.id === remembered) ? remembered : boards[0].id;
      set({ loaded: true });
      showBoard({ boards, activeBoardId, status });
    };

    return {
      boards: [],
      activeBoardId: '',
      widgets: [],
      status: 'loading',
      loaded: false,
      saveError: null,
      editing: false,

      dismissSaveError: () => set({ saveError: null }),

      load: async () => {
        set({ status: 'loading', saveError: null });
        // Nothing this viewer may read — render the default, read-only. ⚠️ This used to be
        // `if (!getToken())`, which was right for My Dashboard and silently wrong for the shared
        // board: its GET is open on a public deployment, so an anonymous visitor was shown the
        // hardcoded default while the admin's composed board sat unread in the database.
        if (!mayLoad(config.readGate, currentViewer())) {
          adopt(config.defaultDoc(), 'ready');
          return;
        }
        // ⚠️ `load()` runs on every mount, and an edit waits `SAVE_DEBOUNCE_MS` before it is sent.
        // Leaving the dashboard and coming back inside that window read the document as it was
        // BEFORE the edit and adopted it; the next edit was then built on that and overwrote the
        // first. Send what is pending, then ask.
        await flushSave();
        const asked = editSeq;
        try {
          const raw = await config.load();
          // An edit made while this was in flight is newer than the answer, and is being saved.
          if (asked !== editSeq) {
            set({ status: 'ready' });
            return;
          }
          // null ⇒ never saved → default. Otherwise sanitize the saved doc (migrates v1, drops
          // retired widget types, clamps spans). A cleared board stays cleared.
          const doc = raw == null ? config.defaultDoc() : sanitizeLayout(raw, registryView);
          adopt(doc, 'ready');
        } catch {
          // 🚨 **Not the default.** Whatever was on screen stays (nothing, on a first load), and
          // `loaded` stays as it was — so a first load that failed leaves a store that refuses
          // to write, and the page offers a retry instead of an edit button.
          set({ status: 'error' });
        }
      },

      setEditing: (on) => {
        if (on) {
          // Snapshot for a possible Cancel. Entering edit mode isn't itself a change → no save.
          editSnapshot = cloneBoards(get().boards);
        } else {
          // Done: keep the live-saved changes; drop the snapshot.
          editSnapshot = null;
        }
        set({ editing: on });
      },

      cancelEditing: () => {
        const snap = editSnapshot;
        editSnapshot = null;
        // No snapshot (shouldn't happen) ⇒ just leave edit mode without touching the layout.
        if (!snap) {
          set({ editing: false });
          return;
        }
        // Restore + persist the snapshot. commit() clears any pending debounced save and schedules
        // the restore, so the final write wins over intermediate mid-edit saves.
        const activeBoardId = snap.some((b) => b.id === get().activeBoardId)
          ? get().activeBoardId
          : snap[0].id;
        commit(cloneBoards(snap), activeBoardId);
        set({ editing: false });
      },

      isDirty: () =>
        editSnapshot != null && JSON.stringify(editSnapshot) !== JSON.stringify(get().boards),

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

      setSize: (instanceId, span, rowSpan) =>
        applyWidgets(setSizeById(get().widgets, instanceId, span, rowSpan, registryView)),

      setSettings: (instanceId, patch) =>
        applyWidgets(setSettingsById(get().widgets, instanceId, patch)),

      renameWidget: (instanceId, title) =>
        applyWidgets(renameWidgetById(get().widgets, instanceId, title)),

      // Reset just the *active* board to the default widget set.
      resetToDefault: () =>
        applyWidgets(config.defaultDoc().boards[0].widgets.map((w) => ({ ...w }))),

      setActiveBoard: (id) => {
        const { boards } = get();
        if (!boards.some((b) => b.id === id)) return;
        // Still no save: switching boards changes no document. It *is* remembered for the session,
        // which is what `showBoard` adds and why this no longer calls `set` directly (ADR-134).
        showBoard({ boards, activeBoardId: id });
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
  readGate: 'session',
  key: 'my',
});

/** Shared Dashboard — one global layout shown to all users. Reads are open; saves are admin-only
 *  (a non-admin save 403s, surfaced as a saveError). The UI gates the customize control to admins. */
export const useSharedLayoutStore = createLayoutStore({
  load: () => api.getSharedDashboard(),
  save: (doc) => api.putSharedDashboard(doc),
  defaultDoc: defaultLayout,
  readGate: 'view',
  key: 'shared',
});

/** The Public Dashboard — the one board an anonymous visitor sees (ADR-123).
 *
 *  🚨 **Not a third presentation store.** What this board carries decides which API routes an
 *  unauthenticated request may reach, so its save is `manage_system` (Admin) where the shared
 *  board's is `manage_config`, and the catalog offers it a narrower set of widgets. Its default is
 *  **empty**, not the five-widget default the other two seed: an unconfigured public board must
 *  show nothing rather than quietly publishing a fleet summary nobody chose to publish. */
export const usePublicLayoutStore = createLayoutStore({
  load: () => api.getPublicDashboard(),
  save: (doc) => api.putPublicDashboard(doc),
  defaultDoc: emptyPublicLayout,
  readGate: 'public',
  key: 'public',
});
