import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// Mock the API client the stores talk to. Declared via vi.hoisted so the mock factory (hoisted
// above imports) can reference them. ApiError mirrors the real one closely enough for instanceof.
const {
  getDashboard,
  putDashboard,
  getSharedDashboard,
  putSharedDashboard,
  getToken,
  ApiError,
} = vi.hoisted(() => {
  class ApiError extends Error {
    status: number;
    constructor(status: number) {
      super(`api ${status}`);
      this.status = status;
    }
  }
  return {
    getDashboard: vi.fn(),
    putDashboard: vi.fn(),
    getSharedDashboard: vi.fn(),
    putSharedDashboard: vi.fn(),
    getToken: vi.fn(),
    ApiError,
  };
});

vi.mock('../services/api', () => ({
  api: { getDashboard, putDashboard, getSharedDashboard, putSharedDashboard },
  getToken,
  ApiError,
}));

import { useLayoutStore, useSharedLayoutStore } from './layoutStore';
import { DASHBOARD_VERSION, sanitizeLayout } from './layout';
import { defaultLayout, getDefinition, registryView } from './registry';

const firstType = defaultLayout().boards[0].widgets[0].type;
const defaultWidgets = () => defaultLayout().boards[0].widgets;

/** Reset a store to a single empty board so widget mutations have an active board to target. */
function reset(store: typeof useLayoutStore) {
  store.setState({
    boards: [{ id: 'b1', name: 'Board 1', widgets: [] }],
    activeBoardId: 'b1',
    widgets: [],
    status: 'loading',
    saveError: null,
    editing: false,
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  // Sensible defaults: authenticated, empty saved layout, saves succeed.
  getToken.mockReturnValue('tok');
  getDashboard.mockResolvedValue(null);
  putDashboard.mockResolvedValue(undefined);
  getSharedDashboard.mockResolvedValue(null);
  putSharedDashboard.mockResolvedValue(undefined);
  reset(useLayoutStore);
  reset(useSharedLayoutStore);
});

afterEach(() => {
  // Discards any pending debounced-save fake timer so it can't leak across tests.
  vi.useRealTimers();
});

describe('useLayoutStore.load', () => {
  it('falls back to the read-only default when unauthenticated (no server call)', async () => {
    getToken.mockReturnValue(null);
    await useLayoutStore.getState().load();
    const s = useLayoutStore.getState();
    expect(s.status).toBe('ready');
    expect(s.widgets).toEqual(defaultWidgets());
    expect(getDashboard).not.toHaveBeenCalled();
  });

  it('uses the default when the server has no saved layout (null)', async () => {
    getDashboard.mockResolvedValue(null);
    await useLayoutStore.getState().load();
    const s = useLayoutStore.getState();
    expect(s.status).toBe('ready');
    expect(s.widgets).toEqual(defaultWidgets());
  });

  it('adopts the saved server layout (sanitized) when present', async () => {
    // A v1-shaped doc (flat widgets) — sanitizeLayout migrates it to a single board.
    const doc = { version: 1, widgets: defaultWidgets().slice(0, 1) };
    getDashboard.mockResolvedValue(doc);
    await useLayoutStore.getState().load();
    const s = useLayoutStore.getState();
    expect(s.status).toBe('ready');
    expect(s.widgets).toEqual(sanitizeLayout(doc, registryView).boards[0].widgets);
    expect(s.widgets).toHaveLength(1);
  });

  it('falls back to default with error status when the fetch fails', async () => {
    getDashboard.mockRejectedValue(new Error('boom'));
    await useLayoutStore.getState().load();
    const s = useLayoutStore.getState();
    expect(s.status).toBe('error');
    expect(s.widgets).toEqual(defaultWidgets());
  });
});

describe('useLayoutStore widget mutations', () => {
  it('addWidget appends a known type and ignores an unknown one', () => {
    useLayoutStore.getState().addWidget(firstType);
    expect(useLayoutStore.getState().widgets).toHaveLength(1);
    expect(useLayoutStore.getState().widgets[0].type).toBe(firstType);

    useLayoutStore.getState().addWidget('not-a-real-widget');
    expect(useLayoutStore.getState().widgets).toHaveLength(1); // unchanged
  });

  it('removeWidget drops the instance by id', () => {
    useLayoutStore.getState().addWidget(firstType);
    const id = useLayoutStore.getState().widgets[0].instanceId;
    useLayoutStore.getState().removeWidget(id);
    expect(useLayoutStore.getState().widgets).toHaveLength(0);
  });

  it('resetToDefault restores the default widgets on the active board', () => {
    useLayoutStore.getState().resetToDefault();
    expect(useLayoutStore.getState().widgets).toEqual(defaultWidgets());
  });

  it('setEditing toggles edit mode', () => {
    useLayoutStore.getState().setEditing(true);
    expect(useLayoutStore.getState().editing).toBe(true);
    useLayoutStore.getState().setEditing(false);
    expect(useLayoutStore.getState().editing).toBe(false);
  });

  it('dismissSaveError clears a previous error', () => {
    useLayoutStore.setState({ saveError: 'something failed' });
    useLayoutStore.getState().dismissSaveError();
    expect(useLayoutStore.getState().saveError).toBeNull();
  });
});

describe('useLayoutStore edit session (snapshot / cancel / resize)', () => {
  it('cancelEditing restores the pre-edit snapshot and persists the restore', async () => {
    const s = useLayoutStore.getState();
    s.setEditing(true); // snapshots the (empty) board
    s.addWidget(firstType);
    expect(useLayoutStore.getState().widgets).toHaveLength(1);
    expect(useLayoutStore.getState().isDirty()).toBe(true);

    useLayoutStore.getState().cancelEditing();
    const after = useLayoutStore.getState();
    expect(after.editing).toBe(false);
    expect(after.widgets).toHaveLength(0); // reverted to the snapshot
    expect(after.isDirty()).toBe(false); // snapshot cleared on leave

    // The restore is persisted (so the intermediate add's debounced save is undone).
    await vi.advanceTimersByTimeAsync(800);
    expect(putDashboard).toHaveBeenCalled();
    expect(putDashboard).toHaveBeenLastCalledWith(
      expect.objectContaining({ boards: [expect.objectContaining({ widgets: [] })] }),
    );
  });

  it('Done (setEditing false) keeps the changes and clears the snapshot', () => {
    const s = useLayoutStore.getState();
    s.setEditing(true);
    s.addWidget(firstType);
    useLayoutStore.getState().setEditing(false);
    const after = useLayoutStore.getState();
    expect(after.widgets).toHaveLength(1); // kept
    expect(after.isDirty()).toBe(false); // no active session
  });

  it('setSize clamps to the widget’s allowed span/rowSpan', () => {
    useLayoutStore.getState().addWidget(firstType);
    const id = useLayoutStore.getState().widgets[0].instanceId;
    useLayoutStore.getState().setSize(id, 999, 999);
    const w = useLayoutStore.getState().widgets[0];
    const def = getDefinition(firstType)!;
    expect(def.allowedSpans).toContain(w.span);
    // rowSpan is present only when the widget allows a taller size and one was chosen.
    if (w.rowSpan !== undefined) expect(def.allowedRowSpans).toContain(w.rowSpan);
  });
});

describe('useLayoutStore board actions', () => {
  it('addBoard appends a new active empty board', () => {
    useLayoutStore.getState().addBoard('Ops');
    const s = useLayoutStore.getState();
    expect(s.boards.map((b) => b.name)).toEqual(['Board 1', 'Ops']);
    expect(s.activeBoardId).toBe(s.boards[1].id);
    expect(s.widgets).toEqual([]); // a fresh board starts empty
  });

  it('setActiveBoard switches the visible board without saving', async () => {
    useLayoutStore.getState().addBoard('Two'); // creates + becomes active (saves)
    await vi.advanceTimersByTimeAsync(800);
    putDashboard.mockClear();

    const firstId = useLayoutStore.getState().boards[0].id;
    useLayoutStore.getState().setActiveBoard(firstId);
    expect(useLayoutStore.getState().activeBoardId).toBe(firstId);
    await vi.advanceTimersByTimeAsync(800);
    expect(putDashboard).not.toHaveBeenCalled();
  });

  it('removeBoard never removes the last board', () => {
    const only = useLayoutStore.getState().boards[0].id;
    useLayoutStore.getState().removeBoard(only);
    expect(useLayoutStore.getState().boards).toHaveLength(1);
  });

  it('removeBoard activates a sibling when the active board is removed', () => {
    useLayoutStore.getState().addBoard('Two'); // active = Two
    const twoId = useLayoutStore.getState().activeBoardId;
    useLayoutStore.getState().removeBoard(twoId);
    const s = useLayoutStore.getState();
    expect(s.boards).toHaveLength(1);
    expect(s.activeBoardId).toBe(s.boards[0].id);
  });

  it('renameBoard renames a board', () => {
    const id = useLayoutStore.getState().boards[0].id;
    useLayoutStore.getState().renameBoard(id, 'Renamed');
    expect(useLayoutStore.getState().boards[0].name).toBe('Renamed');
  });
});

describe('useLayoutStore persistence', () => {
  it('debounces an edit into a single save when authenticated', async () => {
    useLayoutStore.getState().addWidget(firstType);
    expect(putDashboard).not.toHaveBeenCalled(); // not yet — debounced
    await vi.advanceTimersByTimeAsync(800);
    expect(putDashboard).toHaveBeenCalledTimes(1);
    expect(putDashboard).toHaveBeenCalledWith(
      expect.objectContaining({ version: DASHBOARD_VERSION }),
    );
    expect(useLayoutStore.getState().saveError).toBeNull();
  });

  it('coalesces rapid edits into one save', async () => {
    const s = useLayoutStore.getState();
    s.addWidget(firstType);
    s.addWidget(firstType);
    s.addWidget(firstType);
    await vi.advanceTimersByTimeAsync(800);
    expect(putDashboard).toHaveBeenCalledTimes(1);
  });

  it('does not persist edits when unauthenticated', async () => {
    getToken.mockReturnValue(null);
    useLayoutStore.getState().addWidget(firstType);
    await vi.advanceTimersByTimeAsync(800);
    expect(putDashboard).not.toHaveBeenCalled();
  });

  it('surfaces a session-expired message on a 401 save failure', async () => {
    putDashboard.mockRejectedValue(new ApiError(401));
    useLayoutStore.getState().addWidget(firstType);
    await vi.advanceTimersByTimeAsync(800);
    expect(useLayoutStore.getState().saveError).toMatch(/session expired/i);
  });

  it('surfaces a transient retry message on a non-401 save failure', async () => {
    putDashboard.mockRejectedValue(new ApiError(500));
    useLayoutStore.getState().addWidget(firstType);
    await vi.advanceTimersByTimeAsync(800);
    expect(useLayoutStore.getState().saveError).toMatch(/retried/i);
  });
});

describe('store isolation (My Dashboard vs Shared)', () => {
  it('each store saves to its own endpoint only', async () => {
    useLayoutStore.getState().addWidget(firstType);
    await vi.advanceTimersByTimeAsync(800);
    expect(putDashboard).toHaveBeenCalledTimes(1);
    expect(putSharedDashboard).not.toHaveBeenCalled();

    putDashboard.mockClear();
    useSharedLayoutStore.getState().addWidget(firstType);
    await vi.advanceTimersByTimeAsync(800);
    expect(putSharedDashboard).toHaveBeenCalledTimes(1);
    expect(putDashboard).not.toHaveBeenCalled();
  });

  it('the two stores keep independent debounce timers', async () => {
    // A shared module-level timer would let the second edit cancel the first's pending save.
    useLayoutStore.getState().addWidget(firstType);
    useSharedLayoutStore.getState().addWidget(firstType);
    await vi.advanceTimersByTimeAsync(800);
    expect(putDashboard).toHaveBeenCalledTimes(1);
    expect(putSharedDashboard).toHaveBeenCalledTimes(1);
  });

  it('surfaces a permission message on a 403 shared-dashboard save failure', async () => {
    putSharedDashboard.mockRejectedValue(new ApiError(403));
    useSharedLayoutStore.getState().addWidget(firstType);
    await vi.advanceTimersByTimeAsync(800);
    expect(useSharedLayoutStore.getState().saveError).toMatch(/permission/i);
  });
});
