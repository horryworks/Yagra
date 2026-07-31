// SPDX-License-Identifier: AGPL-3.0-only
// Mobile top bar (ADR-027 §2.3): hamburger → home (logo + wordmark) → spacer → notification bell →
// the shared UserMenu. Only mounts in mobile mode (AppShell branch), so it carries no desktop CSS.
// Global search is omitted (it is disabled everywhere today); it returns with the search backend.

import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { useAlertStore } from '../../store';
import { Logo } from './Logo';
import { UserMenu } from './UserMenu';
import './MobileTopBar.css';

interface Props {
  /** Open the navigation drawer (hamburger tap). */
  onOpenMenu: () => void;
}

export function MobileTopBar({ onOpenMenu }: Props) {
  const { t } = useTranslation('nav');
  const navigate = useNavigate();
  const alertCount = useAlertStore((s) => Object.keys(s.alerts).length);

  return (
    <header className="mtopbar">
      <button className="mtopbar-burger" onClick={onOpenMenu} aria-label={t('shell.menu')}>
        <svg width="20" height="20" viewBox="0 0 20 20" aria-hidden fill="none">
          <path
            d="M3 5h14M3 10h14M3 15h14"
            stroke="currentColor"
            strokeWidth="1.6"
            strokeLinecap="round"
          />
        </svg>
      </button>

      <button
        className="mtopbar-home"
        onClick={() => navigate('/dashboard')}
        aria-label={t('shell.home')}
      >
        <Logo size={24} />
        <span className="mtopbar-wordmark">{t('shell.wordmark')}</span>
      </button>

      <div className="mtopbar-right">
        <button
          className="mtopbar-bell"
          onClick={() => navigate('/alerts')}
          title={t('shell.activeAlerts', { count: alertCount })}
          aria-label={t('shell.alerts')}
        >
          <span className="mtopbar-bell-glyph" aria-hidden>
            ◔
          </span>
          {alertCount > 0 && <span className="mtopbar-bell-badge">{alertCount}</span>}
        </button>
        <UserMenu />
      </div>
    </header>
  );
}
