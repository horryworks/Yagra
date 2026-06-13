// Per-type group icons for the inventory tree. Each GroupType renders a distinct inline SVG
// (stroke = currentColor, so it inherits the row's text colour and theming). Decorative —
// aria-hidden; the group's type is also conveyed by its name/label.

import type { ReactNode } from 'react';
import type { GroupType } from '../../types/api';

function Svg({ children }: { children: ReactNode }) {
  return (
    <svg
      className="grp-icon"
      width="15"
      height="15"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {children}
    </svg>
  );
}

/** An icon for a group, chosen by its type (Site / Region / Device type / Service / Generic). */
export function GroupIcon({ type }: { type: GroupType }) {
  switch (type) {
    case 'site': // building
      return (
        <Svg>
          <rect x="3" y="2.5" width="7" height="11" />
          <path d="M10 6h3v7.5M10 9.5h3" />
          <path d="M5 5h1M7.5 5h1M5 7.5h1M7.5 7.5h1M5 10h1M7.5 10h1" />
        </Svg>
      );
    case 'region': // globe
      return (
        <Svg>
          <circle cx="8" cy="8" r="6" />
          <path d="M2 8h12" />
          <path d="M8 2c2.6 2.2 2.6 9.8 0 12c-2.6-2.2-2.6-9.8 0-12z" />
        </Svg>
      );
    case 'device_type': // chip
      return (
        <Svg>
          <rect x="4.5" y="4.5" width="7" height="7" rx="1" />
          <path d="M6.5 1.8v2.7M9.5 1.8v2.7M6.5 11.5v2.7M9.5 11.5v2.7M1.8 6.5h2.7M1.8 9.5h2.7M11.5 6.5h2.7M11.5 9.5h2.7" />
        </Svg>
      );
    case 'service': // stacked layers
      return (
        <Svg>
          <path d="M8 2.2l5.5 2.8L8 7.8L2.5 5z" />
          <path d="M2.5 8L8 10.8L13.5 8" />
          <path d="M2.5 11L8 13.8L13.5 11" />
        </Svg>
      );
    case 'generic':
    default: // folder
      return (
        <Svg>
          <path d="M2 4.6c0-.8.6-1.4 1.4-1.4H6l1.6 1.6h5c.8 0 1.4.6 1.4 1.4v6.2c0 .8-.6 1.4-1.4 1.4H3.4c-.8 0-1.4-.6-1.4-1.4z" />
        </Svg>
      );
  }
}
