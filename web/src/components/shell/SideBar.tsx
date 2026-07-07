// Left sidebar (§2.1, decision 4): the selected section's sub-features, split into labeled groups
// so read/monitor screens sit apart from configuration screens. Collapsible to an icon-only rail.
// Active item = 朱 left border + secondary background. Items whose backend isn't built yet are
// gathered into a trailing "Coming soon" group (dimmed, each marked with a small dot) so the IA
// stays complete without cluttering the working items.

import { NavLink, useLocation } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { sectionForPath, sidebarGroups } from '../../nav';
import { usePrefsStore } from '../../prefs';
import { runningCount, useTroubleshootStore } from '../../troubleshoot/store';
import './SideBar.css';

export function SideBar() {
  const { t } = useTranslation('nav');
  const { pathname } = useLocation();
  const section = sectionForPath(pathname);
  const collapsed = usePrefsStore((s) => s.sidebarCollapsed);
  const toggle = usePrefsStore((s) => s.toggleSidebar);
  // Live "Analysis runs" badge — the number of running troubleshoot jobs (like the TopBar bell
  // badge reads the alert store). Resolved from the item's `liveBadge` discriminator below.
  const runningRuns = useTroubleshootStore((s) => runningCount(s.jobs));
  const groups = sidebarGroups(section);

  return (
    <aside className={collapsed ? 'sidebar collapsed' : 'sidebar'}>
      <div className="sidebar-head">
        {!collapsed && <span className="sidebar-title">{t(section.labelKey)}</span>}
        <button
          className="sidebar-toggle"
          onClick={toggle}
          title={collapsed ? t('shell.expand') : t('shell.collapse')}
          aria-label={collapsed ? t('shell.expandSidebar') : t('shell.collapseSidebar')}
        >
          {collapsed ? '»' : '«'}
        </button>
      </div>
      <nav className="sidebar-nav">
        {groups.map((group, gi) => (
          <div
            className={group.comingSoon ? 'sidebar-group soon' : 'sidebar-group'}
            key={group.labelKey ?? `group-${gi}`}
          >
            {!collapsed && group.labelKey && (
              <div className="sidebar-group-head">{t(group.labelKey)}</div>
            )}
            {group.items.map((item) => (
              <NavLink
                key={item.path}
                to={item.path}
                // `end` so the section-root item (e.g. /nodes) isn't kept active on children.
                end={item.path === section.path || item.path.split('/').length <= 2}
                className={({ isActive }) => (isActive ? 'sidebar-item active' : 'sidebar-item')}
                title={
                  item.descKey ? `${t(item.labelKey)} — ${t(item.descKey)}` : t(item.labelKey)
                }
              >
                {collapsed ? (
                  <span className="sidebar-mono">{item.mono}</span>
                ) : (
                  <>
                    <span className="sidebar-label">{t(item.labelKey)}</span>
                    {item.liveBadge === 'troubleshoot-runs' && runningRuns > 0 && (
                      <span className="sidebar-count">{runningRuns}</span>
                    )}
                    {!item.implemented && (
                      <span className="sidebar-soon" title={t('shell.notImplemented')} />
                    )}
                  </>
                )}
              </NavLink>
            ))}
          </div>
        ))}
      </nav>
    </aside>
  );
}
