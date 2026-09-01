// SPDX-License-Identifier: AGPL-3.0-only
// Placeholder for IA entries whose backend isn't built yet (.claude/docs/design-system.md §5 lists these
// as 🔶 spec-only / ⏸ deferred). The nav entry is intentionally present so the information
// architecture stays whole; this screen states plainly that the backend is pending so it's
// never mistaken for a broken page. Resolves its own title from the nav by current path.

import { useLocation } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { PageHeader } from './PageHeader';
import { Card } from './Card';
import './ComingSoon.css';
import { labelKeysForPath } from '../../nav';

/** Resolve the current path to its nav label keys (or nulls when off-nav). */
export function ComingSoon() {
  const { t } = useTranslation('nav');
  const { pathname } = useLocation();
  const { sectionKey, labelKey } = labelKeysForPath(pathname);
  const label = labelKey ? t(labelKey) : t('shell.comingSoonThisScreen');
  const section = sectionKey ? t(sectionKey) : '';
  const trail = section ? [{ label: section }, { label }] : [{ label }];

  return (
    <div>
      <PageHeader title={label} trail={trail} />
      <Card>
        <div className="comingsoon">
          <div className="comingsoon-badge">{t('shell.comingSoonBadge')}</div>
          <p className="comingsoon-text">{t('shell.comingSoonText', { label })}</p>
        </div>
      </Card>
    </div>
  );
}
