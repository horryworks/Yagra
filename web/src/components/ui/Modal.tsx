// SPDX-License-Identifier: AGPL-3.0-only
// Modal (ui-conventions: use sparingly — confirmation / destructive consent / focused
// editing). One canonical chrome (overlay, radius, header/footer padding, action spacing) so
// every dialog in the app matches — this is the Modals UI-consistency group. Closes on
// overlay click and Escape.

import { useEffect, useId, useRef, useState } from 'react';
import type { CSSProperties, PointerEvent as ReactPointerEvent, KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { FOCUSABLE_SELECTOR, trapTarget } from '../../lib/focusTrap';
import { consumeEscape, escapeClosesDialog } from '../../lib/escapeDismiss';
import {
  MIN_MODAL_H,
  MIN_MODAL_W,
  modalHeightCeiling,
  modalHeightFromKey,
  modalSizeFromDrag,
  modalWidthCeiling,
  modalWidthFromKey,
  resolveModalSize,
  type ModalEdge,
  type ModalResizeId,
  type ModalSize,
} from '../../lib/modalSize';
import { useViewportMode } from '../../lib/viewport';
import { usePrefsStore } from '../../prefs';
import './Modal.css';

interface Props {
  title: ReactNode;
  onClose: () => void;
  /** Footer actions, right-aligned (e.g. Cancel + Confirm). */
  footer?: ReactNode;
  /** Dialog width. `default` is the standard 520px form width; `wide` (~880px) is for content-heavy
   *  dialogs like the report viewer. Mobile always renders full-width (bottom sheet). */
  size?: 'default' | 'wide';
  /** Makes the dialog resizable by its right and bottom edges, remembering the size under this
   *  name in this browser (ADR-198). Only the dialogs named in `MODAL_RESIZE_IDS` take one. */
  resizeId?: ModalResizeId;
  children: ReactNode;
}

/** The window's size, re-read when it changes, so a stored dialog size is re-clamped live. */
function useWindowSize(): { w: number; h: number } {
  const [size, setSize] = useState(() => ({ w: window.innerWidth, h: window.innerHeight }));
  useEffect(() => {
    const onResize = () => setSize({ w: window.innerWidth, h: window.innerHeight });
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, []);
  return size;
}

export function Modal({ title, onClose, footer, size = 'default', resizeId, children }: Props) {
  const { t } = useTranslation('common');
  const titleId = useId();
  const dialogRef = useRef<HTMLDivElement>(null);
  // Resizing (ADR-198): the stored size while no drag is in flight, the drag's own while one is.
  // The store is written once, when the gesture ends. A phone's bottom sheet is never resized.
  const mobile = useViewportMode() === 'mobile';
  const resizable = resizeId !== undefined && !mobile;
  const viewport = useWindowSize();
  const stored = usePrefsStore((s) => (resizeId ? s.modalSizes[resizeId] : undefined));
  const setModalSize = usePrefsStore((s) => s.setModalSize);
  const committed = resolveModalSize(stored, viewport.w, viewport.h);
  const [dragSize, setDragSize] = useState<ModalSize | null>(null);
  const shown: ModalSize = resizable ? (dragSize ?? committed) : { w: null, h: null };
  const gesture = useRef<{ edge: ModalEdge; x: number; y: number; w: number; h: number } | null>(null);
  const measured = () => {
    const box = dialogRef.current?.getBoundingClientRect();
    return { w: box?.width ?? 0, h: box?.height ?? 0 };
  };
  // The drawn size, so a slider that was never dragged still announces a value.
  const [drawn, setDrawn] = useState({ w: 0, h: 0 });
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!resizable || !dialog || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(() => {
      const box = dialog.getBoundingClientRect();
      setDrawn({ w: Math.round(box.width), h: Math.round(box.height) });
    });
    observer.observe(dialog);
    return () => observer.disconnect();
  }, [resizable]);
  const save = (next: ModalSize) => {
    if (resizeId) setModalSize(resizeId, next);
  };
  const onResizeDown = (edge: ModalEdge) => (e: ReactPointerEvent) => {
    e.preventDefault();
    (e.target as Element).setPointerCapture?.(e.pointerId);
    gesture.current = { edge, x: e.clientX, y: e.clientY, ...measured() };
  };
  const dragTo = (g: NonNullable<typeof gesture.current>, x: number, y: number): ModalSize => {
    const next = modalSizeFromDrag(g.edge, { w: g.w, h: g.h }, { x: g.x, y: g.y }, { x, y }, viewport);
    // An axis the edge does not move keeps what was stored, not the measured size: dragging the
    // width must not pin the height the content happened to have.
    return {
      w: g.edge === 'bottom' ? committed.w : next.w,
      h: g.edge === 'right' ? committed.h : next.h,
    };
  };
  const onResizeMove = (e: ReactPointerEvent) => {
    const g = gesture.current;
    if (g) setDragSize(dragTo(g, e.clientX, e.clientY));
  };
  const onResizeUp = (e: ReactPointerEvent) => {
    (e.target as Element).releasePointerCapture?.(e.pointerId);
    const g = gesture.current;
    if (!g) return;
    gesture.current = null;
    // Worked out from where the pointer was released, not from the last move's state, which may
    // not have rendered yet. A cancelled gesture keeps whatever the drag had reached.
    if (e.type === 'pointercancel') {
      if (dragSize) save(dragSize);
    } else if (e.clientX !== g.x || e.clientY !== g.y) {
      save(dragTo(g, e.clientX, e.clientY));
    }
    setDragSize(null);
  };
  const onWidthKey = (e: ReactKeyboardEvent) => {
    const next = modalWidthFromKey(committed.w ?? measured().w, e.key, viewport.w);
    if (next === null) return;
    e.preventDefault();
    save({ ...committed, w: next });
  };
  const onHeightKey = (e: ReactKeyboardEvent) => {
    const next = modalHeightFromKey(committed.h ?? measured().h, e.key, viewport.h);
    if (next === null) return;
    e.preventDefault();
    save({ ...committed, h: next });
  };
  // The size goes in as custom properties and the CSS declares the layout, so the mobile sheet's
  // rules are never beaten by an inline width (ui-conventions, resize handles).
  const sizeStyle = {
    ...(shown.w !== null ? { '--modal-w': `${shown.w}px` } : {}),
    ...(shown.h !== null ? { '--modal-h': `${shown.h}px` } : {}),
  } as CSSProperties;
  const resizeHandlers = (edge: ModalEdge) => ({
    onPointerDown: onResizeDown(edge),
    onPointerMove: onResizeMove,
    onPointerUp: onResizeUp,
    onPointerCancel: onResizeUp,
  });
  // Whether the press that is about to become a click STARTED on the backdrop.
  const pressedOnBackdrop = useRef(false);

  // Escape closes; Tab is contained. Without the containment a dialog is modal only visually —
  // `aria-modal` promises assistive tech that the rest of the page is inert, and tabbing into the
  // form behind an overlay that swallows clicks leaves the keyboard operator editing controls they
  // cannot see. `trapTarget` holds the wrap rules (and is unit-tested); this half is the DOM.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        // Not unconditionally: a popover or menu opened from a control inside this dialog owns the
        // press first, and closing the dialog under it would discard the form (escapeDismiss.ts).
        if (escapeClosesDialog(e)) {
          consumeEscape(e);
          onClose();
        }
        return;
      }
      if (e.key !== 'Tab') return;
      const dialog = dialogRef.current;
      if (!dialog) return;
      // Only the frontmost dialog traps. Two mounted at once (a confirmation raised from an
      // editing dialog) would otherwise each yank focus back into itself on every Tab, which is
      // worse than no trap at all — the keyboard stops working. Document order is mount order.
      const dialogs = document.querySelectorAll('[role="dialog"]');
      if (dialogs.length > 1 && dialogs[dialogs.length - 1] !== dialog) return;
      const focusables = [...dialog.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)];
      const active = document.activeElement;
      const target = trapTarget(focusables, active instanceof HTMLElement ? active : null, e.shiftKey);
      if (target) {
        e.preventDefault();
        target.focus();
      }
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onClose]);

  // The trigger, read while this first renders. 🚨 Not in the effect below: a child's `autoFocus`
  // moves focus during the commit, before any effect runs, so the effect read the dialog's own
  // first field as "what had focus before" and handed focus back to an element that was about to
  // be unmounted. Every dialog whose first field autofocuses — most of them — dropped focus to
  // `<body>` on close (ADR-184 increment 34, `formSubmit.spec.ts`).
  const [trigger] = useState(() => document.activeElement as HTMLElement | null);

  // Move focus into the dialog on open (so keyboard / screen-reader users start inside it) and
  // restore it to the trigger on close. A child `autoFocus` is respected — we only take focus
  // when nothing inside the dialog already has it.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (dialog && !dialog.contains(document.activeElement)) {
      dialog.focus();
    }
    // Only restore focus if the trigger is still in the DOM; if it was removed while the modal
    // was open, calling focus() is a no-op that drops focus to <body>.
    return () => {
      if (trigger?.isConnected) trigger.focus();
    };
  }, [trigger]);

  return (
    // 🚨 **A backdrop click closes; a drag that merely ENDS on the backdrop does not.** The browser
    // dispatches `click` on the nearest common ancestor of where the button went down and where it
    // came up. Selecting text in a field and letting go a few pixels past the dialog's edge is
    // therefore a click whose target is this overlay — the dialog's own `stopPropagation` is not on
    // its path — and a bare `onClick={onClose}` closed the dialog and took every field with it
    // (a pasted CA certificate, a token). Both ends of the gesture have to be on the backdrop.
    <div
      className="modal-overlay"
      onMouseDown={(e) => {
        pressedOnBackdrop.current = e.target === e.currentTarget;
      }}
      onClick={(e) => {
        const dismiss = pressedOnBackdrop.current && e.target === e.currentTarget;
        pressedOnBackdrop.current = false;
        if (dismiss) onClose();
      }}
    >
      <div
        className={[
          'modal',
          size === 'wide' ? 'modal-wide' : '',
          shown.w !== null ? 'has-width' : '',
          shown.h !== null ? 'has-height' : '',
        ]
          .filter(Boolean)
          .join(' ')}
        style={sizeStyle}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        ref={dialogRef}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-head">
          <h2 className="modal-title" id={titleId}>
            {title}
          </h2>
          <button className="modal-close" onClick={onClose} aria-label={t('actions.close')}>
            ×
          </button>
        </div>
        <div className="modal-body">{children}</div>
        {footer && <div className="modal-footer">{footer}</div>}
        {resizable && (
          <>
            {/* The two edges are sliders (ADR-074): announced, arrow-key operable, double-click
                goes back to the default. The corner is a pointer-only shortcut for both. */}
            <div
              className="modal-resize modal-resize-right"
              role="slider"
              tabIndex={0}
              aria-label={t('modalResize.width')}
              aria-orientation="horizontal"
              aria-valuemin={MIN_MODAL_W}
              aria-valuemax={modalWidthCeiling(viewport.w)}
              aria-valuenow={shown.w ?? drawn.w}
              title={t('modalResize.hint')}
              {...resizeHandlers('right')}
              onKeyDown={onWidthKey}
              onDoubleClick={() => save({ ...committed, w: null })}
            />
            <div
              className="modal-resize modal-resize-bottom"
              role="slider"
              tabIndex={0}
              aria-label={t('modalResize.height')}
              aria-orientation="vertical"
              aria-valuemin={MIN_MODAL_H}
              aria-valuemax={modalHeightCeiling(viewport.h)}
              aria-valuenow={shown.h ?? drawn.h}
              title={t('modalResize.hint')}
              {...resizeHandlers('bottom')}
              onKeyDown={onHeightKey}
              onDoubleClick={() => save({ ...committed, h: null })}
            />
            <div
              className="modal-resize modal-resize-corner"
              aria-hidden="true"
              title={t('modalResize.hint')}
              {...resizeHandlers('corner')}
              onDoubleClick={() => save({ w: null, h: null })}
            />
          </>
        )}
      </div>
    </div>
  );
}
