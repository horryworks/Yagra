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

export function ScreenLink({ to }: { to: string }) {
  const { t } = useTranslation('nav');
  const hit = navItemForPath(to);
  if (!hit) return null;
  return (
    <Link to={to} className="screen-link">
      {t(hit.section.labelKey)} ▸ {t(hit.item.labelKey)}
    </Link>
  );
}
