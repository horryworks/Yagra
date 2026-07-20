// SPDX-License-Identifier: AGPL-3.0-only
// Responsive row/card action group. Desktop keeps the familiar hover-revealed IconButton row
// (rendered as a bare fragment so it drops straight into the page's existing `.ytable-actions` /
// `.il-actions` wrapper — the hover-reveal + `(hover:none)` CSS still applies). Mobile collapses
// the same actions behind a single ⋮ "more" button + popover menu, so a card doesn't stack N
// icon buttons at ~390px. The responsive branch lives here so adopting pages just swap their
// inner IconButtons for `<OverflowMenu actions={…} />` — no per-page viewport logic.
//
// Popover mechanics mirror the house pattern (troubleshoot/ToolCard): an `open`-gated effect
// closes on outside mousedown and Escape; the trigger carries aria-haspopup/expanded; items are
// role="menuitem" and stopPropagation() so a clickable row/card underneath isn't triggered.

import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { useViewportMode } from '../../lib/viewport';
import { IconButton } from './IconButton';
import { MoreIcon } from './icons';
import './OverflowMenu.css';

export interface OverflowAction {
  /** Menu-item text + the IconButton title/aria-label. */
  label: string;
  /** Leading glyph — the desktop IconButton child and the mobile menu-item icon. */
  icon: ReactNode;
  onClick: () => void;
  danger?: boolean;
  disabled?: boolean;
}

export function OverflowMenu({ actions }: { actions: OverflowAction[] }) {
  const { t } = useTranslation('common');
  const mobile = useViewportMode() === 'mobile';
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  if (actions.length === 0) return null;

  // Desktop: the original hover-revealed icon row (bare, so the parent wrapper styles it).
  if (!mobile) {
    return (
      <>
        {actions.map((a) => (
          <IconButton
            key={a.label}
            title={a.label}
            danger={a.danger}
            disabled={a.disabled}
            onClick={a.onClick}
          >
            {a.icon}
          </IconButton>
        ))}
      </>
    );
  }

  // Mobile: collapse to a single ⋮ trigger + popover menu.
  return (
    <div className="ovm" ref={ref}>
      <IconButton
        title={t('actions.more')}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((o) => !o);
        }}
      >
        <MoreIcon />
      </IconButton>
      {open && (
        <div className="ovm-menu" role="menu">
          {actions.map((a) => (
            <button
              key={a.label}
              type="button"
              role="menuitem"
              className={a.danger ? 'ovm-item danger' : 'ovm-item'}
              disabled={a.disabled}
              onClick={(e) => {
                e.stopPropagation();
                setOpen(false);
                a.onClick();
              }}
            >
              <span className="ovm-item-icon" aria-hidden>
                {a.icon}
              </span>
              {a.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
