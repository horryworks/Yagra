// SPDX-License-Identifier: AGPL-3.0-only
// A link to another screen, named the way the menu names it: "Settings ▸ Pollers" (ADR-200).
//
// A sentence that pointed somewhere used to spell the menu path out ("Check Settings ▸ Pollers."),
// and the spelling went stale whenever the menu moved. This takes the route and reads both names
// from `nav.ts`, so a renamed item renames every link to it. Inside a translated sentence it is a
// `<Trans>` component: `components={{ lnk: <ScreenLink to="/settings/pollers" /> }}` with `<lnk/>`
// in the string.
//
// `nav.test.ts` fails the build for a `to` the menu does not list, so the `null` below is a guard,
// not a state a screen should reach.

import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { navItemForPath } from '../../nav';
import './ScreenLink.css';

export function ScreenLink({
  to,
  href,
}: {
  to: string;
  /** The same screen on ANOTHER deployment (a relocation's new server). The name still comes from
   *  this build's menu, but the link leaves the app, so it opens in a new tab. */
  href?: string;
}) {
  const { t } = useTranslation('nav');
  const hit = navItemForPath(to);
  if (!hit) return null;
  if (href)
    return (
      <a href={href} className="screen-link" target="_blank" rel="noopener noreferrer">
        {t(hit.section.labelKey)} ▸ {t(hit.item.labelKey)}
      </a>
    );
  return (
    <Link to={to} className="screen-link">
      {t(hit.section.labelKey)} ▸ {t(hit.item.labelKey)}
    </Link>
  );
}
