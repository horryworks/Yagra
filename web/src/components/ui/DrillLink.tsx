// SPDX-License-Identifier: AGPL-3.0-only
// A dashboard part that opens the screen narrowed to it: a ranked row, a donut slice's legend line,
// a KPI figure, a card title (ADR-250).
//
// With no `to`, or where links are switched off (`linksEnabled.ts`), it draws the same content in a
// plain element, so a widget never has to ask either question itself and nothing on the anonymous
// board looks pressable.

import type { ReactNode } from 'react';
import { Link } from 'react-router-dom';
import { useLinksEnabled } from './linksEnabled';
import './DrillLink.css';

interface Props {
  to?: string;
  className?: string;
  title?: string;
  ariaLabel?: string;
  /** Holds block content (a tile), so the plain fallback is a `div` rather than a `span`. */
  block?: boolean;
  children: ReactNode;
}

export function DrillLink({ to, className, title, ariaLabel, block, children }: Props) {
  const on = useLinksEnabled();
  if (!to || !on) {
    const Plain = block ? 'div' : 'span';
    return (
      <Plain className={className} title={title}>
        {children}
      </Plain>
    );
  }
  return (
    <Link
      to={to}
      className={['drill', className].filter(Boolean).join(' ')}
      title={title}
      aria-label={ariaLabel}
    >
      {children}
    </Link>
  );
}
