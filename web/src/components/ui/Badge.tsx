// SPDX-License-Identifier: AGPL-3.0-only
// Small inline label chip (counts, severities, flags). `tone` picks a neutral, status or
// categorical accent; status tones reuse the canonical status palette, never an ad-hoc color.

import type { ReactNode } from 'react';
import './Badge.css';

/** Which colour the chip carries, and therefore what it claims.
 *
 *  `neutral` is the default and the one to reach for: an outline chip that names a category
 *  without asserting anything about it. The four status tones are the canonical status palette
 *  and mean there exactly what they mean everywhere else in the product.
 *
 *  `tag` is the one **filled, non-status** tone (ADR-135 増分 2). It exists because an operator
 *  scanning a node's detail is hunting for *which* labels it carries, and a grey outline chip has
 *  to be read rather than seen. It draws from the categorical family (`--series-*`), so it borrows
 *  neither the orange accent (§1.1 reserves that for active/selected/focus) nor a status colour —
 *  `--tag-bg` in `tokens.css` carries the argument.
 *
 *  ⚠️ **Do not reach for `tag` to make something stand out.** It says "this is a label an operator
 *  hung on this thing". A second filled tone for a different purpose wants its own token and its
 *  own reason, not this one. */
export type Tone = 'neutral' | 'critical' | 'warning' | 'up' | 'info' | 'tag';

export function Badge({
  tone = 'neutral',
  title,
  children,
}: {
  tone?: Tone;
  /** Optional native tooltip (e.g. the raw OID behind a resolved trap name). */
  title?: string;
  children: ReactNode;
}) {
  return (
    <span className={`badge badge-${tone}`} title={title}>
      {children}
    </span>
  );
}
