// SPDX-License-Identifier: AGPL-3.0-only
// Card / pane (§4): the flat surface container (replaces the old `.pane`). Optional title and
// a right-aligned actions slot for a toolbar.

import type { ReactNode } from 'react';
import './Card.css';

interface Props {
  title?: ReactNode;
  actions?: ReactNode;
  className?: string;
  children: ReactNode;
}

export function Card({ title, actions, className, children }: Props) {
  return (
    <section className={['card', className].filter(Boolean).join(' ')}>
      {(title || actions) && (
        <div className="card-head">
          {title && <h2 className="card-title">{title}</h2>}
          {actions && <div className="card-actions">{actions}</div>}
        </div>
      )}
      <div className="card-body">{children}</div>
    </section>
  );
}
