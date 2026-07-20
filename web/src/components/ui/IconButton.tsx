// SPDX-License-Identifier: AGPL-3.0-only
// Square icon-only button for table row actions (edit / delete / copy / toggle). 28×28,
// transparent until hovered; the `danger` variant fills critical-red on hover for destructive
// actions. Always pass a `title` (and it doubles as the accessible name).

import type { ButtonHTMLAttributes, ReactNode } from 'react';
import './IconButton.css';

interface Props extends ButtonHTMLAttributes<HTMLButtonElement> {
  title: string;
  danger?: boolean;
  children: ReactNode;
}

export function IconButton({ title, danger, className, children, ...rest }: Props) {
  const cls = ['icon-btn', danger ? 'danger' : '', className].filter(Boolean).join(' ');
  return (
    <button type="button" className={cls} title={title} aria-label={title} {...rest}>
      {children}
    </button>
  );
}
