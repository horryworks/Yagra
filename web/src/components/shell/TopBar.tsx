// SPDX-License-Identifier: AGPL-3.0-only
// Top bar (§2.1, decision log §6): logo (=home) at the left, text-only section tabs (active
// tab = accent underline), and the always-present right cluster: global search, notification
// bell, user menu. The accent (orange) appears only on the active tab / focus (§1.1).

import { NavLink, useLocation, useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { NAV, sectionForPath, sectionLandingPath } from '../../nav';
import { useAlertStore, useCan, useSectionRouteStore } from '../../store';
import { Logo } from './Logo';
import { UserMenu } from './UserMenu';
import { GlobalSearch } from './GlobalSearch';
import './TopBar.css';

export function TopBar() {
  const { t } = useTranslation('nav');
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const active = sectionForPath(pathname);
  const alertCount = useAlertStore((s) => Object.keys(s.alerts).length);
  // Where each tab goes back to (ADR-134 増分 2). Not `s.path` any more — that constant always
  // landed on the section's first child, so Dashboard could not return to My dashboard.
  const bySection = useSectionRouteStore((s) => s.bySection);
  // The permission composing the public board takes — the same one its handler checks (ADR-056).
  const canSystem = useCan('manage_system');

  return (
    <header className="topbar">
      {/* Home is fixed, and stays fixed while the tabs remember (ADR-134 増分 2 決定 10): when a
          memory takes the operator somewhere unexpected, this is the one control whose destination
          they can predict. The tabs remember; the logo is home. */}
      <button
        className="topbar-home"
        onClick={() => navigate('/dashboard')}
        title={t('shell.home')}
      >
        <Logo />
        <span className="topbar-wordmark">{t('shell.wordmark')}</span>
      </button>

      <nav className="topbar-tabs">
        {NAV.map((s) => (
          <NavLink
            key={s.key}
            to={sectionLandingPath(s, bySection)}
            className={s.key === active.key ? 'topbar-tab active' : 'topbar-tab'}
          >
            {t(s.labelKey)}
          </NavLink>
        ))}
      </nav>

      <div className="topbar-right">
        {/* Global search is a permanent affordance (decision 3). Nodes only for now — the popover
            says so, because a nodes-only result set that looks fleet-wide is worse than none. */}
        <GlobalSearch />
        {/* The bell is a shortcut to Active alerts, not a popover: the count it carries is the
            same set that page lists, so a menu would be a second rendering of one list. */}
        <button
          className="topbar-bell"
          onClick={() => navigate('/alerts')}
          title={t('shell.activeAlerts', { count: alertCount })}
          aria-label={t('shell.alerts')}
        >
          <span className="topbar-bell-glyph" aria-hidden>
            ◔
          </span>
          {alertCount > 0 && <span className="topbar-bell-badge">{alertCount}</span>}
        </button>
        {/* Beside the account badge because that is where "who am I signed in as" lives, and this
            is the one screen about people who are not signed in at all (ADR-123). It is a shortcut
            to the editor, not a preview — the preview toggle is on that page, where the banner can
            explain what it does. Drawn for anyone who may compose the board; ADR-056 forbids
            drawing it disabled for everyone else. */}
        {canSystem && (
          <button
            className="topbar-bell"
            onClick={() => navigate('/dashboard/public')}
            title={t('shell.publicDashboard')}
            aria-label={t('shell.publicDashboard')}
          >
            <span className="topbar-bell-glyph" aria-hidden>
              ◎
            </span>
          </button>
        )}
        <UserMenu />
      </div>
    </header>
  );
}
