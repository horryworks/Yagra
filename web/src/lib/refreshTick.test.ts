import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  REFRESH_TICK_MS,
  getRefreshTick,
  resetRefreshTick,
  subscribeRefreshTick,
} from './refreshTick';

describe('refreshTick', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetRefreshTick();
  });
  afterEach(() => {
    resetRefreshTick();
    vi.useRealTimers();
  });

  it('does not advance while nothing is subscribed', () => {
    vi.advanceTimersByTime(REFRESH_TICK_MS * 3);
    expect(getRefreshTick()).toBe(0);
  });

  it('advances once per interval and wakes the subscriber while subscribed', () => {
    const cb = vi.fn();
    const unsub = subscribeRefreshTick(cb);
    vi.advanceTimersByTime(REFRESH_TICK_MS);
    expect(getRefreshTick()).toBe(1);
    expect(cb).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(REFRESH_TICK_MS);
    expect(getRefreshTick()).toBe(2);
    expect(cb).toHaveBeenCalledTimes(2);
    unsub();
  });

  it('shares ONE timer across subscribers — one increment per interval, not one per subscriber', () => {
    const a = vi.fn();
    const b = vi.fn();
    const ua = subscribeRefreshTick(a);
    const ub = subscribeRefreshTick(b);
    vi.advanceTimersByTime(REFRESH_TICK_MS);
    expect(getRefreshTick()).toBe(1); // a second timer would make this 2
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledTimes(1);
    ua();
    ub();
  });

  it('keeps ticking while any subscriber remains, stops after the last leaves', () => {
    const ua = subscribeRefreshTick(() => {});
    const ub = subscribeRefreshTick(() => {});
    vi.advanceTimersByTime(REFRESH_TICK_MS);
    expect(getRefreshTick()).toBe(1);
    ua(); // one still subscribed
    vi.advanceTimersByTime(REFRESH_TICK_MS);
    expect(getRefreshTick()).toBe(2);
    ub(); // last one gone → frozen
    vi.advanceTimersByTime(REFRESH_TICK_MS * 5);
    expect(getRefreshTick()).toBe(2);
  });

  it('resumes when a new subscriber arrives after all previous ones left', () => {
    const u1 = subscribeRefreshTick(() => {});
    vi.advanceTimersByTime(REFRESH_TICK_MS);
    u1();
    const u2 = subscribeRefreshTick(() => {});
    vi.advanceTimersByTime(REFRESH_TICK_MS);
    expect(getRefreshTick()).toBe(2);
    u2();
  });
});

// The visibility branch only runs when a `document` is present. The node env has none, so — like
// viewport.test — inject a minimal fake document exposing `visibilityState` + a listener registry.
describe('refreshTick — tab visibility', () => {
  const g = globalThis as unknown as {
    document?: {
      visibilityState: string;
      addEventListener: (t: string, cb: () => void) => void;
    };
  };
  let handlers: Record<string, () => void>;

  beforeEach(() => {
    vi.useFakeTimers();
    resetRefreshTick();
    handlers = {};
    g.document = {
      visibilityState: 'visible',
      addEventListener: (type: string, cb: () => void) => {
        handlers[type] = cb;
      },
    };
  });
  afterEach(() => {
    resetRefreshTick();
    delete g.document;
    vi.useRealTimers();
  });

  it('pauses the tick while hidden and catches up immediately on return', () => {
    const cb = vi.fn();
    const unsub = subscribeRefreshTick(cb);
    vi.advanceTimersByTime(REFRESH_TICK_MS);
    expect(getRefreshTick()).toBe(1);

    // Tab hidden → timer stops, no more increments.
    g.document!.visibilityState = 'hidden';
    handlers['visibilitychange']?.();
    vi.advanceTimersByTime(REFRESH_TICK_MS * 3);
    expect(getRefreshTick()).toBe(1);

    // Tab visible again → one immediate catch-up refresh, then ticking resumes.
    g.document!.visibilityState = 'visible';
    handlers['visibilitychange']?.();
    expect(getRefreshTick()).toBe(2);
    vi.advanceTimersByTime(REFRESH_TICK_MS);
    expect(getRefreshTick()).toBe(3);
    unsub();
  });
});
