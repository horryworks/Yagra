// SPDX-License-Identifier: AGPL-3.0-only
// Page header: optional breadcrumb trail, a title, a sub-note, and a right-aligned actions
// slot. Gives every screen a consistent top band.
//
// The sub-note defaults to the screen's nav description (ADR-200): the one line the sidebar shows
// on hover is the one line under the title, so the two cannot drift apart. A screen passes `note`
// only when it has something the nav line cannot say — a count, or a fact that has not found its
// place in the screen yet. Those are listed in `PAGE_NOTES` (`proseBudget.test.ts`).
//
// `writePermission` names what the screen's add / edit controls need. Those controls are not drawn
// for a caller without it (ADR-056), which left a Viewer unable to tell "no such feature" from "not
// mine to use" — so the title carries a "View only" chip that says, when pressed, which privilege
// and which role (ADR-235). Only screens a Viewer may read pass it: on a refused screen
// `LoadBlockNotice` already names the privilege, and "View only" beside it would contradict it.

import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { useLocation } from 'react-router-dom';
import { navItemForPath } from '../../nav';
import { useLacks, usePermissionLabel, useRoleLabel } from '../../store';
import type { Permission } from '../../types/api';
import { Breadcrumb, type Crumb } from '../shell/Breadcrumb';
import { InfoPress } from './InfoTip';
import './PageHeader.css';

interface Props {
  title: ReactNode;
  trail?: Crumb[];
  note?: ReactNode;
  actions?: ReactNode;
  /** The privilege the screen's add / edit controls need; see the header comment. */
  writePermission?: Permission;
}

function ViewOnly({ permission }: { permission: Permission }) {
  const { t } = useTranslation('common');
  const lacks = useLacks(permission);
  const label = usePermissionLabel(permission);
  const role = useRoleLabel();
  if (!lacks) return null;
  return (
    <InfoPress
      infoKey="common:viewOnly.info"
      values={{ permission: label, role }}
      className="badge badge-neutral pageheader-viewonly"
    >
      {t('viewOnly.label')}
    </InfoPress>
  );
}

export function PageHeader({ title, trail, note, actions, writePermission }: Props) {
  const { t } = useTranslation('nav');
  const { pathname } = useLocation();
  const navItem = note === undefined ? navItemForPath(pathname)?.item : undefined;
  const shown = note === undefined && navItem ? t(navItem.descKey) : note;
  return (
    <header className="pageheader">
      {trail && trail.length > 0 && <Breadcrumb trail={trail} />}
      <div className="pageheader-row">
        <div>
          <div className="pageheader-titlerow">
            <h1 className="pageheader-title">{title}</h1>
            {writePermission && <ViewOnly permission={writePermission} />}
          </div>
          {shown && <p className="pageheader-note">{shown}</p>}
        </div>
        {actions && <div className="pageheader-actions">{actions}</div>}
      </div>
    </header>
  );
}
