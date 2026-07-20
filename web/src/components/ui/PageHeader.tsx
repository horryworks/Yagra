// SPDX-License-Identifier: AGPL-3.0-only
// Page header: optional breadcrumb trail, a title, a sub-note, and a right-aligned actions
// slot. Gives every screen a consistent top band.

import type { ReactNode } from 'react';
import { Breadcrumb, type Crumb } from '../shell/Breadcrumb';
import './PageHeader.css';

interface Props {
  title: ReactNode;
  trail?: Crumb[];
  note?: ReactNode;
  actions?: ReactNode;
}

export function PageHeader({ title, trail, note, actions }: Props) {
  return (
    <header className="pageheader">
      {trail && trail.length > 0 && <Breadcrumb trail={trail} />}
      <div className="pageheader-row">
        <div>
          <h1 className="pageheader-title">{title}</h1>
          {note && <p className="pageheader-note">{note}</p>}
        </div>
        {actions && <div className="pageheader-actions">{actions}</div>}
      </div>
    </header>
  );
}
