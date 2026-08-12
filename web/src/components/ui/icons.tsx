// SPDX-License-Identifier: AGPL-3.0-only
// Outline icon set for the data-table standard (search, type glyphs, row actions, etc.).
// Stroke-based, 24×24 viewBox, `currentColor` — they inherit size/color from the element
// they sit in (icon-button, chip, type tile). One small shared set keeps every list aligned.

import type { SVGProps } from 'react';

type IconProps = SVGProps<SVGSVGElement>;

const base = (props: IconProps): IconProps => ({
  viewBox: '0 0 24 24',
  fill: 'none',
  stroke: 'currentColor',
  'aria-hidden': true,
  ...props,
});

export function SearchIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="2" strokeLinecap="round">
      <circle cx="11" cy="11" r="7" />
      <path d="m20 20-3.5-3.5" />
    </svg>
  );
}

export function HashIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.8" strokeLinecap="round">
      <path d="M10 3 8 21M16 3l-2 18M4 8h16M3 16h16" />
    </svg>
  );
}

export function ShieldIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.8" strokeLinejoin="round">
      <path d="M12 3l7 3v6c0 4-3 7-7 9-4-2-7-5-7-9V6z" />
      <path d="M9 12l2 2 4-4" strokeLinecap="round" />
    </svg>
  );
}

export function KeyIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="8" cy="8" r="5" />
      <path d="M11.5 11.5 21 21M17 17l3-3M15 19l2-2" />
    </svg>
  );
}

export function LockIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.8" strokeLinejoin="round">
      <rect x="4" y="11" width="16" height="9" rx="2" />
      <path d="M8 11V8a4 4 0 0 1 8 0v3" />
    </svg>
  );
}

export function EditIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
      <path d="M4 20h4L19 9l-4-4L4 16z" />
      <path d="M14 6l4 4" />
    </svg>
  );
}

export function TrashIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14" />
    </svg>
  );
}

export function CopyIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
      <rect x="9" y="9" width="11" height="11" rx="2" />
      <path d="M5 15V5a2 2 0 0 1 2-2h10" />
    </svg>
  );
}

export function DownloadIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 3v12M7 10l5 5 5-5M4 20h16" />
    </svg>
  );
}

/** Power / enable-disable toggle (used for account status row actions). */
export function PowerIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 3v9" />
      <path d="M7.5 6.5a7 7 0 1 0 9 0" />
    </svg>
  );
}

/** Open box / package — the node-detail eyebrow glyph (a single node in the inventory). */
export function BoxIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
      <path d="M21 8 12 3 3 8v8l9 5 9-5z" />
      <path d="M3 8l9 5 9-5M12 13v8" />
    </svg>
  );
}

/** Calendar — opens the custom date-range popover in RangeControl. */
export function CalendarIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
      <rect x="3.5" y="5" width="17" height="16" rx="2" />
      <path d="M3.5 9.5h17M8 3v4M16 3v4" />
    </svg>
  );
}

/** Wrench — a node/group is in a maintenance window (All Nodes suppression marker). */
export function WrenchIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
      <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />
    </svg>
  );
}

/** Warning triangle with an exclamation — a non-color status cue (paired with text for a11y). */
export function WarningIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 3.5 21.5 20H2.5z" />
      <path d="M12 9.5v4.5" />
      <path d="M12 17.2v.2" strokeWidth="2.2" />
    </svg>
  );
}

/** Bell with a slash — a node/group is muted (notifications suppressed). */
export function BellOffIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
      <path d="M13.73 21a2 2 0 0 1-3.46 0" />
      <path d="M18.63 13A17.89 17.89 0 0 1 18 8" />
      <path d="M6.26 6.26A5.86 5.86 0 0 0 6 8c0 7-3 9-3 9h14" />
      <path d="M18 8a6 6 0 0 0-9.33-5" />
      <path d="M1 1l22 22" />
    </svg>
  );
}

/** Bell, un-slashed — notifications reach this node again (All Nodes: released from a mute).
 *
 * The companion to {@link BellOffIcon}, and the reason it exists: a released marker is drawn as the
 * negation of the active one, and for maintenance that is a struck-through wrench. Striking through
 * a bell that already carries a slash produces two crossing lines that read as "muted" at 16px, so
 * the mute pair negates the other way — the slash comes off instead. */
export function BellIcon(props: IconProps) {
  return (
    <svg {...base(props)} strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
      <path d="M13.73 21a2 2 0 0 1-3.46 0" />
      <path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9" />
    </svg>
  );
}

/** Vertical ellipsis (⋮) — the "more actions" overflow-menu trigger on mobile cards.
 * The shared `base` sets fill:none, so the dots override fill explicitly. */
export function MoreIcon(props: IconProps) {
  return (
    <svg {...base(props)}>
      <circle cx="12" cy="5" r="1.7" fill="currentColor" stroke="none" />
      <circle cx="12" cy="12" r="1.7" fill="currentColor" stroke="none" />
      <circle cx="12" cy="19" r="1.7" fill="currentColor" stroke="none" />
    </svg>
  );
}
