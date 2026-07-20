// SPDX-License-Identifier: AGPL-3.0-only
// Mobile navigation drawer (ADR-027 §2.3): off-canvas left panel that reuses the full nav IA
// (NAV / sidebarGroups / sectionForPath) as a 6-section accordion — the same site map as the
// desktop top bar + sidebar, no duplicate source. The section owning the current route starts
// expanded. Tapping an item navigates and closes; Escape and a backdrop tap also close; focus is
// trapped inside the panel while open. A footer "Desktop view" pins the desktop shell (uiMode).
// Only mounts in mobile mode (AppShell branch).

import { useEffect, useRef, useState } from 'react';
import { NavLink, useLocation } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { NAV, sectionForPath, sidebarGroups } from '../../nav';
import { usePrefsStore } from '../../prefs';
import './MobileNavDrawer.css';

interface Props {
  open: boolean;
  onClose: () => void;
}

export function MobileNavDrawer({ open, onClose }: Props) {
  const { t } = useTranslation('nav');
  const { pathname } = useLocation();
  const active = sectionForPath(pathname);
  const setUiMode = usePrefsStore((s) => s.setUiMode);
  // Which section accordion is expanded (single-open). Defaults to the active route's section.
  const [expanded, setExpanded] = useState(active.key);
  const panelRef = useRef<HTMLDivElement>(null);

  // Re-sync the open accordion to the current route each time the drawer opens.
  useEffect(() => {
    if (open) setExpanded(active.key);
  }, [open, active.key]);

  // While open: Escape closes, Tab is trapped inside the panel, and focus starts on the panel.
  useEffect(() => {
    if (!open) return;
    const panel = panelRef.current;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        onClose();
        return;
      }
      if (e.key !== 'Tab' || !panel) return;
      const focusables = panel.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), [tabindex]:not([tabindex="-1"])',
      );
      if (focusables.length === 0) return;
      const first = focusables[0];
      const last = focusables[focusables.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener('keydown', onKey);
    panel?.focus();
    return () => document.removeEventListener('keydown', onKey);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div className="mdrawer-overlay" onClick={onClose}>
      <div
        className="mdrawer"
        ref={panelRef}
        tabIndex={-1}
        role="dialog"
        aria-modal="true"
        aria-label={t('shell.menu')}
        onClick={(e) => e.stopPropagation()}
      >
        <nav className="mdrawer-nav">
          {NAV.map((section) => {
            const isOpen = expanded === section.key;
            return (
              <div className="mdrawer-section" key={section.key}>
                <button
                  className={isOpen ? 'mdrawer-sec-head open' : 'mdrawer-sec-head'}
                  onClick={() => setExpanded(isOpen ? '' : section.key)}
                  aria-expanded={isOpen}
                >
                  <span>{t(section.labelKey)}</span>
                  <span className="mdrawer-chevron" aria-hidden>
                    {isOpen ? '▾' : '▸'}
                  </span>
                </button>
                {isOpen && (
                  <div className="mdrawer-items">
                    {sidebarGroups(section).map((group, gi) => (
                      <div className="mdrawer-group" key={group.labelKey ?? `g-${gi}`}>
                        {group.labelKey && (
                          <div className="mdrawer-group-head">{t(group.labelKey)}</div>
                        )}
                        {group.items.map((item) => (
                          <NavLink
                            key={item.path}
                            to={item.path}
                            end={item.path === section.path || item.path.split('/').length <= 2}
                            className={({ isActive }) =>
                              isActive ? 'mdrawer-item active' : 'mdrawer-item'
                            }
                            onClick={onClose}
                          >
                            <span className="mdrawer-item-label">{t(item.labelKey)}</span>
                            {!item.implemented && (
                              <span className="mdrawer-soon" title={t('shell.notImplemented')} />
                            )}
                          </NavLink>
                        ))}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            );
          })}
        </nav>
        <div className="mdrawer-foot">
          <button
            className="mdrawer-desktop"
            onClick={() => {
              setUiMode('desktop');
              onClose();
            }}
          >
            {t('shell.desktopView')}
          </button>
        </div>
      </div>
    </div>
  );
}
