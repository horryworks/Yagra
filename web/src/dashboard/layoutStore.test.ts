import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// Mock the API client the store talks to. Declared via vi.hoisted so the mock factory (hoisted
// above imports) can reference them. ApiError mirrors the real one closely enough for instanceof.
const { getDashboard, putDashboard, getToken, ApiError } = vi.hoisted(() => {
  class ApiError extends Error {
    status: number;
    constructor(status: number) {
      super(`api ${status}`);
      this.status = status;
    }
  }
  return { getDashboard: vi.fn(), putDashboard: vi.fn(), getToken: vi.fn(), ApiError };
});

vi.mock('../services/api', () => ({
  api: { getDashboard, putDashboard },
  getToken,
  ApiError,
}));

import { useLayoutStore } from './layoutStore';
import { DASHBOARD_VERSION, sanitizeLayout } from './layout';
import { defaultLayout, registryView } from './registry';

const firstType = defaultLayout().widgets[0].type;

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  // Sensible defaults: authenticated, empty saved layout, saves succeed.
  getToken.mockReturnValue('tok');
  getDashboard.mockResolvedValue(null);
  putDashboard.mockResolvedValue(undefined);
  useLayoutStore.setState({ widgets: [], status: 'loading', saveError: null, editing: false });
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
    expect(s.widgets).toEqual(defaultLayout().widgets);
    expect(getDashboard).not.toHaveBeenCalled();
  });

  it('uses the default when the server has no saved layout (null)', async () => {
    getDashboard.mockResolvedValue(null);
    await useLayoutStore.getState().load();
    const s = useLayoutStore.getState();
    expect(s.status).toBe('ready');
    expect(s.widgets).toEqual(defaultLayout().widgets);
  });

  it('adopts the saved server layout (sanitized) when present', async () => {
    const doc = { version: DASHBOARD_VERSION, widgets: defaultLayout().widgets.slice(0, 1) };
    getDashboard.mockResolvedValue(doc);
    await useLayoutStore.getState().load();
    const s = useLayoutStore.getState();
    expect(s.status).toBe('ready');
    expect(s.widgets).toEqual(sanitizeLayout(doc, registryView).widgets);
    expect(s.widgets).toHaveLength(1);
  });

  it('falls back to default with error status when the fetch fails', async () => {
    getDashboard.mockRejectedValue(new Error('boom'));
    await useLayoutStore.getState().load();
    const s = useLayoutStore.getState();
    expect(s.status).toBe('error');
    expect(s.widgets).toEqual(defaultLayout().widgets);
  });
});

describe('useLayoutStore mutations', () => {
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

  it('resetToDefault restores the default widgets', () => {
    useLayoutStore.getState().resetToDefault();
    expect(useLayoutStore.getState().widgets).toEqual(defaultLayout().widgets);
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
