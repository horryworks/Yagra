// SPDX-License-Identifier: AGPL-3.0-only
// `nodeTreeScroll` — the judgement that keeps the inventory tree where the operator left it
// (ADR-124 増分 5).
//
// Plain objects stand in for the scroller and the press target, which is the whole reason this
// judgement is in a `.ts`: the handler that calls it lives in `NodeTree.tsx`, and Vitest never
// loads a `.tsx` (`.claude/rules/testing.md`). 増分 1 is what a judgement left in one costs — the
// branch that was wrong was the one branch no test could reach.
//
// Two of the tests below are the *accept* side, and they are the ones that make the rest mean
// anything: a guard that pinned the scroller unconditionally would satisfy every "did not move"
// assertion ever written (`rejection-only-tests-pass-when-everything-rejects`).

import { describe, expect, it, vi } from 'vitest';
import {
  FOCUSABLE_IN_ROW,
  focusTargetOf,
  pinFocusScroll,
  restoreScroll,
  scrollAt,
  type Focusable,
  type PressTarget,
  type ScrollAt,
  type ScrollBox,
} from './nodeTreeScroll';

const box = (top: number, left = 0): ScrollBox => ({ scrollTop: top, scrollLeft: left });

/** A press target that resolves to `control` for any selector `FOCUSABLE_IN_ROW` matches. */
const pressOn = (control: Focusable | null): PressTarget => ({
  closest: (selector) => (selector === FOCUSABLE_IN_ROW ? control : null),
});

/** A control that records the options it was focused with. */
function spyControl() {
  const focus = vi.fn();
  return { control: { focus } as Focusable, focus };
}

describe('focusTargetOf', () => {
  it('resolves a press inside a control to that control', () => {
    const { control } = spyControl();
    expect(focusTargetOf(pressOn(control))).toBe(control);
  });

  it('answers null for blank space and for no target at all', () => {
    expect(focusTargetOf(pressOn(null))).toBeNull();
    expect(focusTargetOf(null)).toBeNull();
  });

  // The accept side of the selector. A needle that stopped matching would otherwise report
  // "no control was pressed" for every press, which looks exactly like a well-behaved tree.
  it('names the row controls that exist and the forms a later one might take', () => {
    for (const form of ['button', 'a[href]', 'input', 'select', 'textarea', '[tabindex]']) {
      expect(FOCUSABLE_IN_ROW, `${form} is not covered`).toContain(form);
    }
  });
});

describe('scrollAt', () => {
  it('reads both axes', () => {
    expect(scrollAt(box(120, 40))).toEqual({ top: 120, left: 40 });
  });

  it('answers null before the ref is attached', () => {
    expect(scrollAt(null)).toBeNull();
  });
});

describe('pinFocusScroll', () => {
  // 🚨 This asserts the *option*, not that focus happened. The defect it exists for is a call site
  // that focuses and forgets `preventScroll` — which reads correctly, focuses correctly, and
  // scrolls the pane. `AnchoredPopover.focusPopoverTrigger` shipped exactly that for its whole life.
  it('focuses the pressed control without letting the browser scroll to it', () => {
    const { control, focus } = spyControl();
    let pin: ScrollAt | null | undefined;
    pinFocusScroll(box(120, 40), pressOn(control), (at) => {
      pin = at;
    });
    expect(pin).toEqual({ top: 120, left: 40 });
    expect(focus).toHaveBeenCalledWith({ preventScroll: true });
  });

  // 🚨 THE ORDERING, and it is a real defect this caught rather than a hypothetical. `focus()`
  // dispatches `focusin` synchronously, so the restore handler runs DURING this call. A version
  // that returned the offsets for the caller to assign left the ref null at that moment — the
  // backstop was disarmed by the pre-empt in front of it, and the only visible symptom was the
  // tree still moving 2px on a clipped row.
  it('has recorded the pin before the focus event can fire', () => {
    let pin: ScrollAt | null | undefined;
    let pinAtFocusTime: ScrollAt | null | undefined = undefined;
    const control: Focusable = {
      focus: () => {
        // Stands in for the `focusin` listener, which runs inside `focus()`.
        pinAtFocusTime = pin;
      },
    };
    pinFocusScroll(box(137), pressOn(control), (at) => {
      pin = at;
    });
    expect(pinAtFocusTime, 'the focus handler ran before the pin was recorded').toEqual({
      top: 137,
      left: 0,
    });
  });

  // 🚨 THE STALE-PIN BRANCH. A pin taken from *any* press is never consumed when the press focused
  // nothing — pressing the scrollbar fires no `click` on the body either — and the next keyboard
  // focus would then restore an offset from before the operator's own wheel.
  it('pins nothing when the press landed on blank space', () => {
    const { focus } = spyControl();
    let pin: ScrollAt | null | undefined = { top: 9, left: 9 };
    pinFocusScroll(box(120), pressOn(null), (at) => {
      pin = at;
    });
    expect(pin, 'a stale pin survived a press on blank space').toBeNull();
    expect(focus).not.toHaveBeenCalled();
  });
});

describe('restoreScroll', () => {
  it('puts a scroller the browser nudged back where it was', () => {
    const el = box(137);
    const pin = scrollAt(el);
    // What the browser does when a half-clipped row's button takes focus.
    el.scrollTop = 149;
    expect(restoreScroll(el, pin)).toBe(true);
    expect(el.scrollTop).toBe(137);
  });

  it('restores the horizontal axis too', () => {
    const el = box(0, 80);
    const pin = scrollAt(el);
    el.scrollLeft = 210;
    expect(restoreScroll(el, pin)).toBe(true);
    expect(el.scrollLeft).toBe(80);
  });

  // The half that makes the `true` above mean something: a caller that wrote unconditionally would
  // pass every assertion except this one.
  it('writes nothing, and says so, when nothing moved', () => {
    const el = box(120, 40);
    let writes = 0;
    const watched: ScrollBox = {
      get scrollTop() {
        return el.scrollTop;
      },
      set scrollTop(v) {
        writes += 1;
        el.scrollTop = v;
      },
      get scrollLeft() {
        return el.scrollLeft;
      },
      set scrollLeft(v) {
        writes += 1;
        el.scrollLeft = v;
      },
    };
    expect(restoreScroll(watched, scrollAt(watched))).toBe(false);
    expect(writes, 'an unchanged offset was written back anyway').toBe(0);
  });

  // The accept side: a keyboard Tab into a row has no pin, and the browser scrolling the target
  // into view is then the CORRECT behaviour. Undoing it would make the tree unreachable by
  // keyboard, which is a worse bug than the one this module exists for.
  it('leaves a keyboard focus alone', () => {
    const el = box(300);
    expect(restoreScroll(el, null)).toBe(false);
    expect(el.scrollTop).toBe(300);
  });

  it('is a no-op before the ref is attached', () => {
    expect(restoreScroll(null, { top: 10, left: 0 })).toBe(false);
  });
});
