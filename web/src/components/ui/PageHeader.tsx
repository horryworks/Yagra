// SPDX-License-Identifier: AGPL-3.0-only
// Page header: optional breadcrumb trail, a title, a sub-note, and a right-aligned actions
// slot. Gives every screen a consistent top band.
//
// The sub-note defaults to the screen's nav description (ADR-200): the one line the sidebar shows
// on hover is the one line under the title, so the two cannot drift apart. A screen passes `note`
// only when it has something the nav line cannot say — a count, or a fact that has not found its
// place in the screen yet. Those are listed in `PAGE_NOTES` (`proseBudget.test.ts`).

import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { useLocation } from 'react-router-dom';
import { navItemForPath } from '../../nav';
import { Breadcrumb, type Crumb } from '../shell/Breadcrumb';
import './PageHeader.css';

interface Props {
  title: ReactNode;
  trail?: Crumb[];
  note?: ReactNode;
  actions?: ReactNode;
}

export function PageHeader({ title, trail, note, actions }: Props) {
  const { t } = useTranslation('nav');
  const { pathname } = useLocation();
  const navItem = note === undefined ? navItemForPath(pathname)?.item : undefined;
  const shown = note === undefined && navItem ? t(navItem.descKey) : note;
  return (
    <header className="pageheader">
      {trail && trail.length > 0 && <Breadcrumb trail={trail} />}
      <div className="pageheader-row">
        <div>
          <h1 className="pageheader-title">{title}</h1>
          {shown && <p className="pageheader-note">{shown}</p>}
        </div>
        {actions && <div className="pageheader-actions">{actions}</div>}
      </div>
    </header>
  );
}
