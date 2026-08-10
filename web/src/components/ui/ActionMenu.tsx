// SPDX-License-Identifier: AGPL-3.0-only
// Anchored dropdown menu: a caller-supplied trigger that opens a list of actions.
//
// This is the primitive `web/src/components/ui/` was missing. `OverflowMenu` is NOT one — on
// desktop it deliberately renders a bare IconButton row and no menu at all — so the popover chrome
// had been re-typed three times (`.ovm-menu`, troubleshoot's `.ts-run-menu`, NodeTree's
// `.ntree-menu`). Those three are follow-ups, not this change: `OverflowMenu`'s mobile branch is
// the natural first adopter but is live on 14+ pages with no component tests to catch a regression
// (`.tsx` tests never run — see .claude/rules/testing.md), and `.ntree-menu` is a *different*
// primitive (cursor-anchored context menu carrying sections and chips). Migrate them here rather
// than growing a fourth copy.
//
// The menu is `fixed`, measured from the trigger, and rendered through a PORTAL to document.body.
// Every part of that is load-bearing. The callers sit in `.nodes-pane`, which is `overflow: hidden`
// inside a 312px grid column, so an absolutely-positioned menu is clipped whichever edge it aligns
// to. `fixed` escapes that — but only while no ancestor establishes a containing block for fixed
// descendants, and a virtualized list does exactly that: `@tanstack/react-virtual` gives every row
// a `transform: translateY(...)`, against which viewport coordinates silently resolve. Rendered in
// place inside a tree row the menu therefore landed far outside the scroller, invisible, wide
// enough to raise a horizontal scrollbar. The portal is what makes "fixed" mean the viewport
// again, and it is why this component is safe to put inside any row — including a `DataTable` one.
//
// Behaviour follows the house pattern (troubleshoot/ToolCard): outside-mousedown and Escape close,
// the trigger carries aria-haspopup/aria-expanded, items are role="menuitem" and stopPropagation()
// so a clickable row underneath isn't triggered. On top of that it implements the WAI-ARIA
// menu-button keyboard contract, because a menu that only opens by mouse is not operable.

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import type { KeyboardEvent, MouseEvent, ReactNode } from 'react';
import './ActionMenu.css';

export interface ActionMenuItem {
  /** Stable identity. Not the label: two items may legitimately read the same. */
  key: string;
  label: string;
  onSelect: () => void;
  /** Optional leading glyph. */
  icon?: ReactNode;
  danger?: boolean;
  disabled?: boolean;
}

/** What the caller must spread onto its own button. */
export interface ActionMenuTriggerProps {
  'aria-haspopup': 'menu';
  'aria-expanded': boolean;
  onClick: (e: MouseEvent<HTMLElement>) => void;
  onKeyDown: (e: KeyboardEvent<HTMLElement>) => void;
}

interface Props {
  items: ActionMenuItem[];
  /** Accessible name for the popover itself (`aria-label` on role="menu"). */
  label: string;
  /** Which trigger edge the menu aligns to before it is clamped to the viewport. */
  align?: 'start' | 'end';
  /** Render prop: the caller keeps its own Button/IconButton and styling, and the ARIA state
   *  lands on whichever element that is. */
  trigger: (props: ActionMenuTriggerProps) => ReactNode;
  className?: string;
  /** Told whenever the menu opens or closes. For a trigger that is only conditionally visible —
   *  a hover-revealed row action — which must stay rendered while its own menu is open, or the
   *  menu disappears the moment the pointer leaves the row to reach it. */
  onOpenChange?: (open: boolean) => void;
}

/** Gap between trigger and menu, and the minimum distance kept from the viewport edge. */
const GAP = 4;
const EDGE = 8;

export function ActionMenu({
  items,
  label,
  align = 'end',
  trigger,
  className,
  onOpenChange,
}: Props) {
  const [open, setOpen] = useState(false);
  /** Measured position. `null` until the layout effect has run — the menu stays invisible until
   *  then, so it never paints at the wrong place first. */
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  /** Item to focus once the menu is measured: an index, -1 for the last one, `null` for none.
   *  A pointer open moves no focus — the operator is already looking where they clicked, and
   *  focusing scrolls the item into view, which scrolls any scrollable ancestor the trigger sits
   *  in. A keyboard open must move focus, or the menu is unreachable. */
  const focusOnOpen = useRef<number | null>(null);
  /** Set by Enter/Space on the trigger so the native click that follows knows it was a keyboard
   *  activation. Cleared by that click. */
  const openedByKey = useRef(false);

  /** The trigger element, found by the attribute this component itself supplies — so it cannot
   *  drift, and no `forwardRef` is needed on Button/IconButton (neither has one). */
  const triggerEl = useCallback(
    () => wrapRef.current?.querySelector<HTMLElement>('[aria-haspopup="menu"]') ?? null,
    [],
  );

  const enabled = items.map((it, i) => (it.disabled ? -1 : i)).filter((i) => i >= 0);
  const focusAt = (n: number) => itemRefs.current[enabled[n]]?.focus();

  /** Measure the trigger and clamp the menu to the viewport. */
  const place = useCallback(() => {
    const t = triggerEl()?.getBoundingClientRect();
    const m = menuRef.current?.getBoundingClientRect();
    if (!t || !m) return;
    const maxLeft = Math.max(EDGE, window.innerWidth - m.width - EDGE);
    const left = Math.min(Math.max(align === 'start' ? t.left : t.right - m.width, EDGE), maxLeft);
    let top = t.bottom + GAP;
    if (top + m.height > window.innerHeight - EDGE) top = Math.max(EDGE, t.top - m.height - GAP);
    setPos({ top, left });
  }, [align, triggerEl]);

  // Runs before paint, so the first frame is already in the right place.
  useLayoutEffect(() => {
    if (open) place();
  }, [open, place]);

  // Move focus into the menu once it is positioned (a hidden element cannot take focus).
  useLayoutEffect(() => {
    if (!open || !pos || focusOnOpen.current === null || enabled.length === 0) return;
    const want = focusOnOpen.current;
    focusOnOpen.current = null;
    focusAt(want === -1 ? enabled.length - 1 : Math.min(want, enabled.length - 1));
  });

  // Reported from one place rather than at each of the five sites that flip `open` — a missed one
  // would leave the trigger's row stuck visible, or hide the menu the operator is reading.
  const openChangeRef = useRef(onOpenChange);
  openChangeRef.current = onOpenChange;
  useEffect(() => {
    openChangeRef.current?.(open);
  }, [open]);
  // A conditionally-rendered trigger (a virtualized row scrolled out of the window) unmounts this
  // whole component, so say so — otherwise the caller's "keep this row visible" flag sticks on a
  // row that no longer has a menu.
  useEffect(() => () => openChangeRef.current?.(false), []);

  useEffect(() => {
    if (!open) return;
    const shut = (restoreFocus: boolean) => {
      setOpen(false);
      setPos(null);
      if (restoreFocus) triggerEl()?.focus();
    };
    const onDown = (e: globalThis.MouseEvent) => {
      // The menu is portalled, so it is NOT inside `wrapRef` in the DOM — both have to be asked,
      // or clicking a menu item closes the menu before its handler runs.
      const t = e.target as Node;
      if (!wrapRef.current?.contains(t) && !menuRef.current?.contains(t)) shut(false);
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === 'Escape') shut(true);
    };
    // Capture phase, because scroll does not bubble: the menu is positioned from the trigger's
    // rect, so it has to be re-placed whenever an ancestor scrolls. It must NOT close instead —
    // a trigger inside a scroll container (a tree row) scrolls its own ancestor when the menu
    // takes focus, which closed the menu in the frame it opened.
    const onMove = () => place();
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    window.addEventListener('scroll', onMove, true);
    window.addEventListener('resize', onMove);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
      window.removeEventListener('scroll', onMove, true);
      window.removeEventListener('resize', onMove);
    };
  }, [open, place, triggerEl]);

  if (items.length === 0) return null;

  const openWith = (focus: number | null) => {
    focusOnOpen.current = focus;
    setOpen(true);
  };

  const triggerProps: ActionMenuTriggerProps = {
    'aria-haspopup': 'menu',
    'aria-expanded': open,
    onClick: (e) => {
      e.stopPropagation();
      const viaKey = openedByKey.current;
      openedByKey.current = false;
      if (open) {
        setOpen(false);
        setPos(null);
      } else {
        openWith(viaKey ? 0 : null);
      }
    },
    onKeyDown: (e) => {
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault();
        const last = e.key === 'ArrowUp';
        // Already open (a pointer open leaves focus on the trigger) — step into the menu.
        if (open) focusAt(last ? enabled.length - 1 : 0);
        else openWith(last ? -1 : 0);
      } else if (e.key === 'Enter' || e.key === ' ') {
        // Let the native activation fire the click; it reads this to know to move focus.
        openedByKey.current = true;
      }
    },
  };

  const onMenuKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (enabled.length === 0) return;
    const cur = enabled.indexOf(itemRefs.current.findIndex((el) => el === document.activeElement));
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      focusAt(cur < 0 ? 0 : (cur + 1) % enabled.length);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      focusAt(cur < 0 ? enabled.length - 1 : (cur - 1 + enabled.length) % enabled.length);
    } else if (e.key === 'Home') {
      e.preventDefault();
      focusAt(0);
    } else if (e.key === 'End') {
      e.preventDefault();
      focusAt(enabled.length - 1);
    } else if (e.key === 'Tab') {
      // Close and let focus move on — Tab must leave the menu, not walk it.
      setOpen(false);
      setPos(null);
    }
  };

  return (
    <div className={['amenu', className].filter(Boolean).join(' ')} ref={wrapRef}>
      {trigger(triggerProps)}
      {open &&
        createPortal(
          <div
            ref={menuRef}
            className="amenu-menu"
            role="menu"
            aria-label={label}
            style={
              pos ? { top: pos.top, left: pos.left } : { top: 0, left: 0, visibility: 'hidden' }
            }
            onKeyDown={onMenuKeyDown}
            onClick={(e) => e.stopPropagation()}
          >
            {items.map((it, i) => (
              <button
                key={it.key}
                type="button"
                role="menuitem"
                tabIndex={-1}
                ref={(el) => {
                  itemRefs.current[i] = el;
                }}
                className={it.danger ? 'amenu-item danger' : 'amenu-item'}
                disabled={it.disabled}
                onClick={(e) => {
                  e.stopPropagation();
                  setOpen(false);
                  setPos(null);
                  it.onSelect();
                }}
              >
                {it.icon && (
                  <span className="amenu-item-icon" aria-hidden>
                    {it.icon}
                  </span>
                )}
                <span className="amenu-item-label">{it.label}</span>
              </button>
            ))}
          </div>,
          document.body,
        )}
    </div>
  );
}
