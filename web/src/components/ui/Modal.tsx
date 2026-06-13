// Modal (ui-conventions: use sparingly — confirmation / destructive consent / focused
// editing). One canonical chrome (overlay, radius, header/footer padding, action spacing) so
// every dialog in the app matches — this is the Modals UI-consistency group. Closes on
// overlay click and Escape.

import { useEffect, useId, useRef } from 'react';
import type { ReactNode } from 'react';
import './Modal.css';

interface Props {
  title: ReactNode;
  onClose: () => void;
  /** Footer actions, right-aligned (e.g. Cancel + Confirm). */
  footer?: ReactNode;
  children: ReactNode;
}

export function Modal({ title, onClose, footer, children }: Props) {
  const titleId = useId();
  const dialogRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onClose]);

  // Move focus into the dialog on open (so keyboard / screen-reader users start inside it) and
  // restore it to the trigger on close. A child `autoFocus` is respected — we only take focus
  // when nothing inside the dialog already has it.
  useEffect(() => {
    const previouslyFocused = document.activeElement as HTMLElement | null;
    const dialog = dialogRef.current;
    if (dialog && !dialog.contains(document.activeElement)) {
      dialog.focus();
    }
    return () => previouslyFocused?.focus?.();
  }, []);

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div
        className="modal"
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
          <button className="modal-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </div>
        <div className="modal-body">{children}</div>
        {footer && <div className="modal-footer">{footer}</div>}
      </div>
    </div>
  );
}
