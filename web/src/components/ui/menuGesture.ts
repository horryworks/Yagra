// SPDX-License-Identifier: AGPL-3.0-only
// The one way to give an element a right-click menu (ADR-271 decision 5): the right button, and a
// long press for a finger, from one call.
//
//   const gesture = useMenuGesture();
//   <div {...gesture.bind(hasMenu ? (at) => setMenu({ ...at, … }) : null)}>
//
// A bare `onContextMenu=` is refused by `contextMenu.test.ts`, because a menu wired that way is
// a menu no iPhone can open — iOS Safari fires no `contextmenu` on a long press. The rule itself
// (how long, how still, which events a press swallows) is `lib/longPress.ts`; this file only feeds
// it the element's events and owns the timer.
//
// ⚠️ A right-click opens the menu at the pointer; a long press opens it just below the element
// (`menuPointBelow`), so the finger neither covers it nor lifts on one of its items.
// ⚠️ One `useMenuGesture` per surface, not per row: there is one finger press at a time, and a
// hook cannot be called inside a row-rendering function anyway.

import { useCallback, useEffect, useMemo, useRef } from 'react';
import type {
  MouseEvent as ReactMouseEvent,
  PointerEvent as ReactPointerEvent,
  TouchEvent as ReactTouchEvent,
} from 'react';
import { IDLE, LONG_PRESS_MS, menuPointBelow, stepPress } from '../../lib/longPress';
import type { PressEffect, PressEvent, PressState } from '../../lib/longPress';

/** A point in viewport coordinates — what `AnchoredPopover`'s `at` takes. */
export interface MenuPoint {
  x: number;
  y: number;
}

/** Opens the element's menu at `at`. */
export type OpenMenu = (at: MenuPoint) => void;

/** What `bind` spreads onto the element. */
export interface MenuGestureProps {
  onContextMenu?: (e: ReactMouseEvent<HTMLElement>) => void;
  onPointerDown?: (e: ReactPointerEvent<HTMLElement>) => void;
  onPointerMove?: (e: ReactPointerEvent<HTMLElement>) => void;
  onPointerUp?: (e: ReactPointerEvent<HTMLElement>) => void;
  onPointerCancel?: (e: ReactPointerEvent<HTMLElement>) => void;
  onTouchEnd?: (e: ReactTouchEvent<HTMLElement>) => void;
}

export interface MenuGesture {
  /** The handlers for one element. `null` when it has no menu right now: nothing is bound, and a
   *  right-click there gets the browser's own menu, as before. */
  bind: (open: OpenMenu | null) => MenuGestureProps;
  /** A finger is down on a bound element, or its press has just opened a menu. A drag that starts
   *  now is the long press, not a move — the caller cancels it. */
  pressing: () => boolean;
}

/** The clock the timer and the events share. */
const now = (): number => performance.now();

export function useMenuGesture(): MenuGesture {
  const state = useRef<PressState>(IDLE);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  /** The element being pressed and the menu it opens — read when the timer fires. */
  const pressed = useRef<{ el: HTMLElement; open: OpenMenu } | null>(null);

  const disarm = useCallback(() => {
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = null;
  }, []);

  useEffect(() => disarm, [disarm]);

  const step = useCallback((e: PressEvent): PressEffect => {
    const [next, effect] = stepPress(state.current, e);
    state.current = next;
    if (effect === 'disarm' || effect === 'open') disarm();
    return effect;
  }, [disarm]);

  const openBelow = useCallback(() => {
    const p = pressed.current;
    // A row the virtualizer has since dropped has nowhere to put its menu.
    if (!p || !p.el.isConnected) return;
    p.open(menuPointBelow(p.el.getBoundingClientRect()));
  }, []);

  const bind = useCallback(
    (open: OpenMenu | null): MenuGestureProps => {
      if (!open) return {};
      const up = (e: ReactPointerEvent<HTMLElement>) => {
        step({ type: 'up', pointerId: e.pointerId, at: now() });
      };
      return {
        onPointerDown: (e) => {
          const effect = step({
            type: 'down',
            pointerId: e.pointerId,
            pointerType: e.pointerType,
            isPrimary: e.isPrimary,
            x: e.clientX,
            y: e.clientY,
            at: now(),
          });
          const s = state.current;
          if (effect !== 'arm' || s.phase !== 'pressing') return;
          disarm();
          pressed.current = { el: e.currentTarget, open };
          const armedFor = s.since;
          timer.current = setTimeout(() => {
            timer.current = null;
            if (step({ type: 'timer', at: now(), armedFor }) === 'open') openBelow();
          }, LONG_PRESS_MS);
        },
        onPointerMove: (e) => {
          step({ type: 'move', pointerId: e.pointerId, x: e.clientX, y: e.clientY, at: now() });
        },
        onPointerUp: up,
        onPointerCancel: up,
        onContextMenu: (e) => {
          const effect = step({ type: 'contextmenu', at: now() });
          if (effect === 'none') return;
          e.preventDefault();
          if (effect === 'open') openBelow();
          else if (effect === 'native') open({ x: e.clientX, y: e.clientY });
        },
        // A touch event goes to the element the finger first touched, so this sees the lift even
        // when the menu has opened over the row. Cancelling it means no mousedown and no click:
        // the menu stays open, no item is pressed, and the row is not selected.
        onTouchEnd: (e) => {
          if (step({ type: 'touchend', at: now() }) === 'swallow') e.preventDefault();
        },
      };
    },
    [step, disarm, openBelow],
  );

  return useMemo(() => ({ bind, pressing: () => state.current.phase !== 'idle' }), [bind]);
}
