// Small inline label chip (counts, severities, flags). `tone` picks a neutral or status
// accent; status tones reuse the canonical status palette, never an ad-hoc color.

import type { ReactNode } from 'react';
import './Badge.css';

type Tone = 'neutral' | 'critical' | 'warning' | 'up' | 'info';

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
