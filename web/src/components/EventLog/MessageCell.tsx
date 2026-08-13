// SPDX-License-Identifier: AGPL-3.0-only
// The Events table's Message cell: one truncated line, the matched parts marked, and the full text
// in a popover (ADR-053 Inc.2e).
//
// **Why this is a component and not a `render:` arrow like every other column.** It has to hold
// open/closed state and an anchor ref, and `Column.render` is a plain function — hooks cannot live
// there. The cost is one component; the alternative was lifting per-row popover state into the page,
// which would re-render the whole table on every hover.
//
// **Why a popover rather than the `title` it replaces.** The native tooltip was the previous answer
// and it is a poor one: ~1s to appear, gone after a few seconds, unselectable, and long syslog lines
// are truncated by the platform — these messages run past 400 characters. It also cannot show the
// marks, which is the point of the exercise. The `title` is removed rather than kept, or the OS
// tooltip fades in *over* the popover.
//
// **Hover is not enough on its own.** `ui-conventions.md` forbids a new hover-only affordance —
// there is no hover on a touch screen — so a click opens it too, and a click while open pins it so
// the text can be selected and copied. Escape and an outside click close it either way.
//
// ⚠️ **The portal is load-bearing.** A virtualized row carries `transform: translateY(...)`, which
// becomes the containing block for `position: fixed`, and `.dt` is `overflow: hidden`. A panel
// rendered in place lands off-screen with a stray horizontal scrollbar as the only clue — a symptom
// this repo has already paid for once. `AnchoredPopover` owns that and needs no change here: it is
// fully controlled, so "when is it open" stays a decision of this file.

import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AnchoredPopover } from '../ui/AnchoredPopover';
import { Badge } from '../ui/Badge';
import { Marked } from '../ui/Marked';
import type { MatchSemantics } from '../../lib/matchRanges';
import type { TextCondition } from '../../lib/filterCondition';
import type { EventRow } from '../../types/api';

/** What the row was matched on, so the marks can say the same thing the query asked. Built once per
 *  query by `eventColumns`, never per row. */
export interface MessageHighlight {
  cond: TextCondition | null;
  semantics: MatchSemantics;
  widened: boolean;
}

/** Long enough that a deliberate move onto a message opens it, short enough that crossing the table
 *  on the way somewhere else does not. */
const OPEN_MS = 250;
/** Short, and it exists only so the pointer can cross the gap between the cell and the panel. */
const CLOSE_MS = 150;

export function MessageCell({ row, highlight }: { row: EventRow; highlight?: MessageHighlight }) {
  const { t } = useTranslation('alerts');
  const [open, setOpen] = useState(false);
  /** Pinned by a click: the pointer leaving no longer closes it, so the text can be selected. */
  const [pinned, setPinned] = useState(false);
  const anchorRef = useRef<HTMLSpanElement | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  // A pending open must not fire after the row has scrolled away and been recycled onto different
  // data — the virtualizer reuses these components.
  useEffect(() => () => clearTimeout(timer.current), []);

  const schedule = (next: boolean, ms: number) => {
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setOpen(next), ms);
  };

  const close = () => {
    clearTimeout(timer.current);
    setPinned(false);
    setOpen(false);
  };

  return (
    <span
      className="events-msg-cell"
      ref={anchorRef}
      onMouseEnter={() => !pinned && schedule(true, OPEN_MS)}
      onMouseLeave={() => !pinned && schedule(false, CLOSE_MS)}
    >
      {row.trap_name && (
        <Badge tone="info" title={row.trap_oid ?? undefined}>
          {row.trap_name}
        </Badge>
      )}
      <button
        type="button"
        className="events-msg mono"
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => {
          clearTimeout(timer.current);
          // Click is a toggle *of the pinned state*: the first one keeps open what hover opened.
          if (pinned) close();
          else {
            setPinned(true);
            setOpen(true);
          }
        }}
      >
        <Marked text={row.message} highlight={highlight} />
      </button>
      {open && (
        <AnchoredPopover
          open={open}
          anchorRef={anchorRef}
          role="dialog"
          align="start"
          className="events-msg-pop"
          label={t('eventLog.cols.message')}
          onDismiss={close}
        >
          <p className="events-msg-full mono">
            <Marked text={row.message} highlight={highlight} />
          </p>
        </AnchoredPopover>
      )}
    </span>
  );
}
