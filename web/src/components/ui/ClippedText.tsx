// SPDX-License-Identifier: AGPL-3.0-only
// Data text in a table cell: one line, an ellipsis where it is cut, and the whole string on hover
// (ADR-088 Inc.4).
//
// The ellipsis is the half that went missing. `text-overflow` does nothing on a flex or grid box,
// because the text inside becomes an anonymous flex item, so a cell whose content element was
// `display: flex` cut its text with no mark at all. The operator could not tell anything was
// missing, so a `title` beside it was a tooltip nobody had a reason to look for. This box is
// always `display: block`; put a flex layout around it, never on it.
//
// The `title` is set whether or not the text is cut. Measuring first would cost a resize observer
// per cell, and a tooltip repeating short visible text does no harm.

import type { ReactNode } from 'react';
import './ClippedText.css';

export function ClippedText({
  text,
  className,
  children,
}: {
  /** The whole string. It is the tooltip, and what is drawn unless `children` is given. */
  text: string;
  className?: string;
  /** What to draw instead of the bare text — a link, say. It must read as `text`. */
  children?: ReactNode;
}) {
  return (
    <span className={className ? `clipped-text ${className}` : 'clipped-text'} title={text}>
      {children ?? text}
    </span>
  );
}
