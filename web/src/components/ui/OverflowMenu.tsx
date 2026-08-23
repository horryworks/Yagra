// SPDX-License-Identifier: AGPL-3.0-only
// Responsive row/card action group. Desktop keeps the familiar hover-revealed IconButton row
// (rendered as a bare fragment so it drops straight into the page's existing `.ytable-actions` /
// `.il-actions` wrapper — the hover-reveal + `(hover:none)` CSS still applies). Mobile collapses
// the same actions behind a single ⋮ "more" button + popover menu, so a card doesn't stack N
// icon buttons at ~390px. The responsive branch lives here so adopting pages just swap their
// inner IconButtons for `<OverflowMenu actions={…} />` — no per-page viewport logic.
//
// **The mobile menu is `ActionMenu`, and it had to become one.** It used to be a hand-rolled
// popover: `position: absolute; top: calc(100% + 4px); right: 0`, no measurement and no clamp. That
// is fine until the row is at the bottom or the left of the list, and on a phone it was neither
// fine nor visible — ADR-088's browser check opened the **last** row's menu on all twelve screens
// that use this component and found it broken on ten. Nine were clipped away entirely, because
// `DataTable`'s mobile card renders its values in `.dt-card-v { overflow: hidden }` and an
// absolutely-positioned child cannot escape a clipping ancestor; the tenth opened 123px off the
// left edge of a 390px screen, because `right: 0` aligns a 176px panel to a trigger that is not
// 176px from the left. Nothing saw any of it: the route walk runs at 1280px, where this component
// renders no popover at all.
//
// `ActionMenu` (via `AnchoredPopover`) portals to `document.body` and clamps to the viewport, which
// is precisely those two failures. Its own header had named this branch "the natural first adopter"
// and deferred it for one stated reason — "no component tests to catch a regression". ADR-088 Inc.3
// is that test, so the reason expired and the migration is that same commit.

import { useTranslation } from 'react-i18next';
import type { ReactNode } from 'react';
import { useViewportMode } from '../../lib/viewport';
import { ActionMenu } from './ActionMenu';
import { IconButton } from './IconButton';
import { MoreIcon } from './icons';

export interface OverflowAction {
  /** Menu-item text + the IconButton title/aria-label. Also the item's identity: every caller's
   *  actions read differently from each other (Edit / Delete / Enable …), which is what makes this
   *  safe as `ActionMenuItem.key`. */
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

  // Mobile: collapse to a single ⋮ trigger + menu. `ActionMenu` owns the popover, the placement,
  // Escape/outside-click and the WAI-ARIA menu-button keyboard contract; what stays here is the
  // trigger's own look and the mapping from this component's action shape to a menu item.
  return (
    <ActionMenu
      label={t('actions.more')}
      items={actions.map((a) => ({
        key: a.label,
        label: a.label,
        onSelect: a.onClick,
        icon: a.icon,
        danger: a.danger,
        disabled: a.disabled,
      }))}
      trigger={(props) => (
        <IconButton title={t('actions.more')} {...props}>
          <MoreIcon />
        </IconButton>
      )}
    />
  );
}
