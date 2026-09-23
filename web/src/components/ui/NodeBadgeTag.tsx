// SPDX-License-Identifier: AGPL-3.0-only
// One badge after a node's name, as every list that shows one draws it — the inventory tree, a
// folder's members, the node header and the move dialog. Each keeps its own class (a component owns
// its own CSS file); what they share is here, so a badge drawn as a glyph cannot be one in three
// places and a word in the fourth.

import { brandBadgeClass } from '../../lib/brandBadge';
import { badgeIconClass, type NodeBadge } from '../../lib/nodeKind';
import { WifiIcon } from './icons';

/** `className` is the list's own badge class; `label` is the translated `badge.labelKey`. */
export function NodeBadgeTag({
  badge,
  className,
  label,
}: {
  badge: NodeBadge;
  className: string;
  label: string;
}) {
  const cls = `${className}${brandBadgeClass(badge.brand)}${badgeIconClass(badge.icon)}`;
  switch (badge.icon) {
    // A glyph has no text for a screen reader, so the badge is an image named by its tooltip.
    case 'wifi':
      return (
        <span className={cls} title={label} role="img" aria-label={label}>
          <WifiIcon className="badge-glyph" />
        </span>
      );
    case null:
      return (
        <span className={cls} title={label}>
          {badge.text}
        </span>
      );
    default: {
      // A new `BADGE_ICONS` entry is a compile error here until it has a glyph.
      const unknown: never = badge.icon;
      return unknown;
    }
  }
}
