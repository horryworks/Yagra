// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  IDLE,
  LONG_PRESS_LIFT_GUARD_MS,
  LONG_PRESS_MS,
  LONG_PRESS_SLOP_PX,
  MENU_ROW_INSET_PX,
  menuPointBelow,
  stepPress,
} from './longPress';
import type { PressEffect, PressEvent, PressState } from './longPress';

const down = (at: number, extra: Partial<Extract<PressEvent, { type: 'down' }>> = {}): PressEvent => ({
  type: 'down',
  pointerId: 1,
  pointerType: 'touch',
  isPrimary: true,
  x: 100,
  y: 200,
  at,
  ...extra,
});

const timer = (at: number, armedFor = 0): PressEvent => ({ type: 'timer', at, armedFor });

/** Feed the events in order; return the last state and every effect. */
function run(events: PressEvent[], from: PressState = IDLE): [PressState, PressEffect[]] {
  let s = from;
  const effects: PressEffect[] = [];
  for (const e of events) {
    const [next, eff] = stepPress(s, e);
    s = next;
    effects.push(eff);
  }
  return [s, effects];
}

describe('stepPress', () => {
  it('opens the menu when a finger stays down for the whole press', () => {
    const [s, effects] = run([down(0), timer(LONG_PRESS_MS)]);
    expect(effects).toEqual(['arm', 'open']);
    expect(s.phase).toBe('opened');
  });

  it('does not open for a finger lifted before the press is long enough', () => {
    const [, effects] = run([
      down(0),
      { type: 'up', pointerId: 1, at: 400 },
      timer(LONG_PRESS_MS),
      { type: 'touchend', at: 410 },
    ]);
    expect(effects).toEqual(['arm', 'disarm', 'none', 'none']);
  });

  it('ignores a timer started for an earlier press', () => {
    const [, effects] = run([
      down(0),
      { type: 'up', pointerId: 1, at: 100 },
      down(300),
      timer(500, 0),
      timer(800, 300),
    ]);
    expect(effects).toEqual(['arm', 'disarm', 'arm', 'none', 'open']);
  });

  it('treats a finger that drifts past the slop as a scroll', () => {
    const [, effects] = run([
      down(0),
      { type: 'move', pointerId: 1, x: 100 + LONG_PRESS_SLOP_PX, y: 200, at: 100 },
      { type: 'move', pointerId: 1, x: 100, y: 215, at: 200 },
      timer(LONG_PRESS_MS),
    ]);
    // Within the slop it is still a press; 15px is a scroll.
    expect(effects).toEqual(['arm', 'none', 'disarm', 'none']);
  });

  it('gives up when the browser takes the touch for a scroll (pointercancel)', () => {
    const [, effects] = run([down(0), { type: 'up', pointerId: 1, at: 120 }, timer(500)]);
    expect(effects).toEqual(['arm', 'disarm', 'none']);
  });

  it('gives up when a second finger lands', () => {
    const [s, effects] = run([
      down(0),
      down(50, { pointerId: 2, isPrimary: false }),
      timer(LONG_PRESS_MS),
    ]);
    expect(effects).toEqual(['arm', 'disarm', 'none']);
    expect(s).toEqual(IDLE);
  });

  it('leaves a mouse to its right button', () => {
    const [, effects] = run([
      down(0, { pointerType: 'mouse' }),
      timer(LONG_PRESS_MS),
      { type: 'contextmenu', at: 600 },
    ]);
    expect(effects).toEqual(['none', 'none', 'native']);
  });

  it('takes a pen like a finger', () => {
    expect(run([down(0, { pointerType: 'pen' }), timer(500)])[1]).toEqual([
      'arm',
      'open',
    ]);
  });

  it('cancels the lift of the press that opened the menu, and only that one', () => {
    const [, effects] = run([
      down(0),
      timer(500),
      { type: 'up', pointerId: 1, at: 900 },
      { type: 'touchend', at: 910 },
      { type: 'touchend', at: 1000 },
    ]);
    expect(effects).toEqual(['arm', 'open', 'none', 'swallow', 'none']);
  });

  it("swallows the browser's own long press arriving just after the lift, and nothing later", () => {
    const lift = 900;
    const late = (at: number) =>
      run([down(0), timer(500), { type: 'up', pointerId: 1, at: lift }, { type: 'contextmenu', at }])[1].at(-1);
    expect(late(lift + LONG_PRESS_LIFT_GUARD_MS)).toBe('swallow');
    expect(late(lift + LONG_PRESS_LIFT_GUARD_MS + 1)).toBe('native');
  });

  it('claims the lift however long the finger stayed down', () => {
    const [, effects] = run([down(0), timer(500), { type: 'touchend', at: 5000 }]);
    expect(effects.at(-1)).toBe('swallow');
  });

  it("lets a tap right after the menu opened through, rather than eat the tap's lift", () => {
    const [, effects] = run([
      down(0),
      timer(500),
      { type: 'up', pointerId: 1, at: 600 },
      down(700, { pointerId: 2 }),
      { type: 'up', pointerId: 2, at: 760 },
      { type: 'touchend', at: 770 },
    ]);
    expect(effects).toEqual(['arm', 'open', 'none', 'arm', 'disarm', 'none']);
  });

  it("opens from the browser's own long press when it comes first, once (Android)", () => {
    const [, effects] = run([
      down(0),
      { type: 'contextmenu', at: 450 },
      timer(500),
      { type: 'up', pointerId: 1, at: 800 },
      { type: 'touchend', at: 810 },
    ]);
    expect(effects).toEqual(['arm', 'open', 'none', 'none', 'swallow']);
  });

  it("swallows the browser's long press when the timer came first", () => {
    const [, effects] = run([down(0), timer(500), { type: 'contextmenu', at: 550 }]);
    expect(effects).toEqual(['arm', 'open', 'swallow']);
  });

  it('opens an ordinary right-click at its own point', () => {
    expect(run([{ type: 'contextmenu', at: 10 }])[1]).toEqual(['native']);
  });
});

describe('menuPointBelow', () => {
  it("puts the menu under the row, in from the row's left edge", () => {
    expect(menuPointBelow({ left: 12, bottom: 230 })).toEqual({ x: 12 + MENU_ROW_INSET_PX, y: 230 });
  });
});
