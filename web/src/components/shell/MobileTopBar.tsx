// SPDX-License-Identifier: AGPL-3.0-only
// Mobile top bar (ADR-027 §2.3): hamburger → home (logo + wordmark) → spacer → notification bell →
// the shared UserMenu. Only mounts in mobile mode (AppShell branch), so it carries no desktop CSS.
// Global search opens as a full-width row under the bar rather than living in it: at 44px targets
// the bar has no room for a field, and a tap-to-open row keeps the burger/home/bell reachable.

import { useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { useAlertStore } from '../../store';
import { Logo } from './Logo';
import { UserMenu } from './UserMenu';
import { GlobalSearch } from './GlobalSearch';
import './MobileTopBar.css';

interface Props {
  /** Open the navigation drawer (hamburger tap). */
  onOpenMenu: () => void;
}

export function MobileTopBar({ onOpenMenu }: Props) {
  const { t } = useTranslation('nav');
  const navigate = useNavigate();
  const alertCount = useAlertStore((s) => Object.keys(s.alerts).length);
  const [searchOpen, setSearchOpen] = useState(false);
  const searchRef = useRef<HTMLDivElement>(null);
  const toggleRef = useRef<HTMLButtonElement>(null);

  // The row could only be closed by finding the ⌕ again (ADR-073). Escape and a tap outside now do
  // it too, which is what every other transient surface in the app already offered. `GlobalSearch`
  // handles its own Escape for the results popover; both fire on one press, so a single Escape
  // dismisses the search outright rather than leaving an empty row behind.
  useEffect(() => {
    if (!searchOpen) return;
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (searchRef.current?.contains(target) || toggleRef.current?.contains(target)) return;
      setSearchOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setSearchOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [searchOpen]);

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
          ref={toggleRef}
          className="mtopbar-bell"
          onClick={() => setSearchOpen((v) => !v)}
          aria-label={t('shell.globalSearch')}
          aria-expanded={searchOpen}
        >
          <span className="mtopbar-bell-glyph" aria-hidden>
            ⌕
          </span>
        </button>
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

      {searchOpen && (
        <div className="mtopbar-search" ref={searchRef}>
          <GlobalSearch mobile />
        </div>
      )}
    </header>
  );
}
