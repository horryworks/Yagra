// Left sidebar (§2.1, decision 4): the selected section's sub-features. Collapsible to an
// icon-only rail. Active item = 朱 left border + secondary background. Items whose backend
// isn't built yet still render (routing to ComingSoon) so the IA stays complete, marked with
// a small dot so the operator knows it's a placeholder.

import { NavLink, useLocation } from 'react-router-dom';
import { sectionForPath } from '../../nav';
import { usePrefsStore } from '../../prefs';
import { runningCount, useTroubleshootStore } from '../../troubleshoot/store';
import './SideBar.css';

export function SideBar() {
  const { pathname } = useLocation();
  const section = sectionForPath(pathname);
  const collapsed = usePrefsStore((s) => s.sidebarCollapsed);
  const toggle = usePrefsStore((s) => s.toggleSidebar);
  // Live "Analysis runs" badge — the number of running troubleshoot jobs (like the TopBar bell
  // badge reads the alert store). Resolved from the item's `liveBadge` discriminator below.
  const runningRuns = useTroubleshootStore((s) => runningCount(s.jobs));

  return (
    <aside className={collapsed ? 'sidebar collapsed' : 'sidebar'}>
      <div className="sidebar-head">
        {!collapsed && <span className="sidebar-title">{section.label}</span>}
        <button
          className="sidebar-toggle"
          onClick={toggle}
          title={collapsed ? 'Expand' : 'Collapse'}
          aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
        >
          {collapsed ? '»' : '«'}
        </button>
      </div>
      <nav className="sidebar-nav">
        {section.items.map((item) => (
          <NavLink
            key={item.path}
            to={item.path}
            // `end` so the section-root item (e.g. /nodes) isn't kept active on children.
            end={item.path === section.path || item.path.split('/').length <= 2}
            className={({ isActive }) => (isActive ? 'sidebar-item active' : 'sidebar-item')}
            title={item.label}
          >
            {collapsed ? (
              <span className="sidebar-mono">{item.mono}</span>
            ) : (
              <>
                <span className="sidebar-label">{item.label}</span>
                {item.liveBadge === 'troubleshoot-runs' && runningRuns > 0 && (
                  <span className="sidebar-count">{runningRuns}</span>
                )}
                {!item.implemented && <span className="sidebar-soon" title="Not implemented yet" />}
              </>
            )}
          </NavLink>
        ))}
      </nav>
    </aside>
  );
}
