// Top bar (§2.1, decision log §6): logo (=home) at the left, text-only section tabs (active
// tab = 朱 underline), and the always-present right cluster: global search, notification
// bell, user menu. 朱 (accent) appears only on the active tab / focus (§1.1).

import { NavLink, useLocation, useNavigate } from 'react-router-dom';
import { NAV, sectionForPath } from '../../nav';
import { useAlertStore } from '../../store';
import { Logo } from './Logo';
import { UserMenu } from './UserMenu';
import './TopBar.css';

export function TopBar() {
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const active = sectionForPath(pathname);
  const alertCount = useAlertStore((s) => Object.keys(s.alerts).length);

  return (
    <header className="topbar">
      <button className="topbar-home" onClick={() => navigate('/dashboard')} title="Home">
        <Logo />
        <span className="topbar-wordmark">Yagra</span>
      </button>

      <nav className="topbar-tabs">
        {NAV.map((s) => (
          <NavLink
            key={s.key}
            to={s.path}
            className={s.key === active.key ? 'topbar-tab active' : 'topbar-tab'}
          >
            {s.label}
          </NavLink>
        ))}
      </nav>

      <div className="topbar-right">
        {/* Global search is a permanent affordance (decision 3). A search endpoint does not
            exist yet, so it is present but disabled for now. */}
        <input
          className="topbar-search"
          type="search"
          placeholder="Search…"
          disabled
          title="Search is not available yet"
          aria-label="Global search"
        />
        <button className="topbar-bell" title={`${alertCount} active alerts`} aria-label="Alerts">
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
