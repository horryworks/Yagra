// SPDX-License-Identifier: AGPL-3.0-only
// A long press with a finger is a right-click (ADR-271).
//
// iOS Safari fires no `contextmenu` on a long press, so on an iPhone every right-click menu in the
// WebUI was out of reach. Android Chrome does fire one, after a delay of its own choosing. This is
// the one rule for both: a finger (or a pen) held still for `LONG_PRESS_MS` opens the menu, and
// whichever of our timer or the browser's own `contextmenu` comes first opens it — the other is
// swallowed, so the menu never opens twice and the browser's own menu never shows.
//
// Pure, so every rule below has a test (`longPress.test.ts`). The timers and the DOM live in
// `components/ui/menuGesture.ts`, which feeds this the events it sees.

/** How long a finger must stay down. The platforms' own long press is about this long. */
export const LONG_PRESS_MS = 500;

/** How far a finger may drift and still be "held still". Past it, the gesture is a scroll. */
export const LONG_PRESS_SLOP_PX = 10;

/** How long after the finger lifts the press still claims what the browser sends for it — a late
 *  `contextmenu` of the browser's own must not open the menu a second time. */
export const LONG_PRESS_LIFT_GUARD_MS = 700;

/** How far in from a row's left edge a menu opened below the row appears — past the twisty and the
 *  icon, where a right-click on the name would have put it (ADR-155 decision 7). */
export const MENU_ROW_INSET_PX = 40;

export type PressState =
  | { phase: 'idle' }
  /** A finger is down and has not moved; the timer is running. */
  | { phase: 'pressing'; pointerId: number; x: number; y: number; since: number }
  /** The press opened the menu. `liftedAt` is null while the finger is still down. */
  | { phase: 'opened'; pointerId: number; liftedAt: number | null };

export const IDLE: PressState = { phase: 'idle' };

/** What the element saw. `up` covers `pointerup` and `pointercancel` alike — a press the browser
 *  took for a scroll ends the same way as one the finger let go of. `touchend` is the lift as touch
 *  events see it: cancelling it is what stops the browser turning the lift into a click. Every
 *  event carries the time, read from the same clock as the timer. */
export type PressEvent =
  | {
      type: 'down';
      pointerId: number;
      pointerType: string;
      isPrimary: boolean;
      x: number;
      y: number;
      at: number;
    }
  | { type: 'move'; pointerId: number; x: number; y: number; at: number }
  | { type: 'up'; pointerId: number; at: number }
  /** `armedFor` is the `since` of the press the timer was started for, so a timer left over from
   *  an earlier press can never open the menu for a later one. */
  | { type: 'timer'; at: number; armedFor: number }
  | { type: 'contextmenu'; at: number }
  | { type: 'touchend'; at: number };

/**
 * What the caller does about it.
 *
 * - `arm` / `disarm`: start or stop the timer.
 * - `open`: open the menu below the pressed element, and stop the timer.
 * - `swallow`: cancel the event — it belongs to a press that already opened the menu.
 * - `native`: an ordinary right-click (mouse or keyboard); open the menu at the event's point.
 * - `none`: let the event through.
 */
export type PressEffect = 'none' | 'arm' | 'disarm' | 'open' | 'swallow' | 'native';

/** Only a finger and a pen. A mouse has a right button. */
export function isPressPointer(pointerType: string): boolean {
  return pointerType === 'touch' || pointerType === 'pen';
}

/** Whether a press that opened the menu still claims what comes next. */
function stillClaims(s: Extract<PressState, { phase: 'opened' }>, at: number): boolean {
  return s.liftedAt === null || at - s.liftedAt <= LONG_PRESS_LIFT_GUARD_MS;
}

/** One step of the long-press rule: the next state, and what to do about the event. */
export function stepPress(state: PressState, e: PressEvent): [PressState, PressEffect] {
  // A press that opened the menu and was let go of long ago claims nothing any more.
  const s: PressState = state.phase === 'opened' && !stillClaims(state, e.at) ? IDLE : state;

  switch (e.type) {
    case 'down': {
      if (!isPressPointer(e.pointerType)) return [s, 'none'];
      // A second finger is a pinch or a two-finger scroll, never a press.
      if (!e.isPrimary) return s.phase === 'pressing' ? [IDLE, 'disarm'] : [s, 'none'];
      // A new touch ends whatever the last press claimed: a tap right after the menu opened is a
      // tap, and its click must go through.
      return [{ phase: 'pressing', pointerId: e.pointerId, x: e.x, y: e.y, since: e.at }, 'arm'];
    }
    case 'move': {
      if (s.phase !== 'pressing' || s.pointerId !== e.pointerId) return [s, 'none'];
      return Math.hypot(e.x - s.x, e.y - s.y) > LONG_PRESS_SLOP_PX ? [IDLE, 'disarm'] : [s, 'none'];
    }
    case 'up': {
      if (s.phase === 'pressing' && s.pointerId === e.pointerId) return [IDLE, 'disarm'];
      if (s.phase === 'opened' && s.pointerId === e.pointerId && s.liftedAt === null) {
        return [{ ...s, liftedAt: e.at }, 'none'];
      }
      return [s, 'none'];
    }
    case 'timer': {
      if (s.phase !== 'pressing' || s.since !== e.armedFor) return [s, 'none'];
      return [{ phase: 'opened', pointerId: s.pointerId, liftedAt: null }, 'open'];
    }
    case 'contextmenu': {
      // The browser's own long press came before our timer (Android): it opens the menu, and the
      // timer then finds nothing to do.
      if (s.phase === 'pressing') {
        return [{ phase: 'opened', pointerId: s.pointerId, liftedAt: null }, 'open'];
      }
      if (s.phase === 'opened') return [s, 'swallow'];
      return [s, 'native'];
    }
    case 'touchend': {
      // 🚨 Cancel the lift of a press that opened the menu. Left alone, a browser may send the
      // lift on as a mousedown and a click at the finger's point — where, on a short screen, the
      // menu now is: the menu closes on the mousedown, or the click presses one of its items.
      // Chromium's touch emulation does exactly that (`treeLongPress.spec.ts`).
      if (s.phase === 'opened') return [IDLE, 'swallow'];
      return [s, 'none'];
    }
  }
}

/** Where a menu opened for a row goes: just below it, so the finger that opened it does not cover
 *  it and does not lift on one of its items. The keyboard opens a row's menu at the same place. */
export function menuPointBelow(box: { left: number; bottom: number }): { x: number; y: number } {
  return { x: box.left + MENU_ROW_INSET_PX, y: box.bottom };
}
