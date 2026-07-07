// Top bar (§2.1, decision log §6): logo (=home) at the left, text-only section tabs (active
// tab = 朱 underline), and the always-present right cluster: global search, notification
// bell, user menu. 朱 (accent) appears only on the active tab / focus (§1.1).

import { NavLink, useLocation, useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { NAV, sectionForPath } from '../../nav';
import { useAlertStore } from '../../store';
import { Logo } from './Logo';
import { UserMenu } from './UserMenu';
import './TopBar.css';

export function TopBar() {
  const { t } = useTranslation('nav');
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const active = sectionForPath(pathname);
  const alertCount = useAlertStore((s) => Object.keys(s.alerts).length);

  return (
    <header className="topbar">
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
            to={s.path}
            className={s.key === active.key ? 'topbar-tab active' : 'topbar-tab'}
          >
            {t(s.labelKey)}
          </NavLink>
        ))}
      </nav>

      <div className="topbar-right">
        {/* Global search is a permanent affordance (decision 3). A search endpoint does not
            exist yet, so it is present but disabled for now. */}
        <input
          className="topbar-search"
          type="search"
          placeholder={t('shell.search')}
          disabled
          title={t('shell.searchUnavailable')}
          aria-label={t('shell.globalSearch')}
        />
        <button
          className="topbar-bell"
          title={t('shell.activeAlerts', { count: alertCount })}
          aria-label={t('shell.alerts')}
        >
          <span className="topbar-bell-glyph" aria-hidden>
            ◔
          </span>
          {alertCount > 0 && <span className="topbar-bell-badge">{alertCount}</span>}
        </button>
        <UserMenu />
      </div>
    </header>
  );
}
