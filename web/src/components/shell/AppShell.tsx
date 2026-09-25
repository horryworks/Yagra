// SPDX-License-Identifier: AGPL-3.0-only
// App shell: top bar + collapsible sidebar + routed content (§2 IA). The content region owns
// its own scroll so the chrome stays fixed and live lists/tables scroll within their pane.
//
// Mobile (ADR-027, viewport < 768px or a forced desktop→auto): a mobile top bar + off-canvas
// drawer replace the desktop top bar + sidebar; the routed content is unchanged (it adapts via the
// shared breakpoint rules). The desktop branch is byte-for-byte its previous self.
//
// The Troubleshoot SSE stream and its toast are mounted here rather than on the three Troubleshoot
// pages, so a "notify me" run still reports its completion after the operator has navigated away —
// which is the whole point of asking to be notified.
//
// The alert stream is mounted here for the same reason (ADR-019 増分 1): the top-bar bell reads the
// alert store on every screen, and while only four pages subscribed it read zero everywhere else,
// or froze at whatever it was when the operator left a dashboard.

import { useEffect, useState } from 'react';
import { Outlet, useLocation } from 'react-router-dom';
import { SideBar } from './SideBar';
import { TopBar } from './TopBar';
import { MobileTopBar } from './MobileTopBar';
import { MobileNavDrawer } from './MobileNavDrawer';
import { rememberableRoute } from '../../nav';
import { useSectionRouteStore } from '../../store';
import { useViewportMode } from '../../lib/viewport';
import { useTroubleshootStream } from '../../troubleshoot/useTroubleshootStream';
import { TroubleshootToast } from '../../troubleshoot/TroubleshootToast';
import { useAlertStream } from '../../hooks/useAlertStream';
import './AppShell.css';

export function AppShell() {
  const mobile = useViewportMode() === 'mobile';
  const [drawerOpen, setDrawerOpen] = useState(false);
  const { pathname, search } = useLocation();
  const rememberRoute = useSectionRouteStore((s) => s.rememberRoute);
  useTroubleshootStream();
  useAlertStream();

  // Close the drawer on any route change — covers the browser Back button and a tap on the
  // already-current route (which wouldn't change the pathname otherwise).
  useEffect(() => {
    setDrawerOpen(false);
  }, [pathname]);

  // Remember where each nav section and each menu item was last visited, so the top-bar tab and the
  // sidebar item both return here (ADR-134 増分 2 and 3). Recorded from the *route*, not from a
  // nav click: a redirect, the bell's shortcut and a shared link all land the operator somewhere
  // real, and that somewhere is their current position (決定 12). `rememberableRoute` returns null
  // for anything the menu does not declare, which is what keeps a node detail from becoming the
  // Nodes tab's destination.
  //
  // Above the mobile branch on purpose: both shells record the same way, and the mobile drawer's
  // items read the same per-item memory as the sidebar's.
  useEffect(() => {
    const hit = rememberableRoute(pathname, search);
    if (hit) rememberRoute(hit.sectionKey, hit.itemPath, hit.route);
  }, [pathname, search, rememberRoute]);

  if (mobile) {
    return (
      <div className="shell">
        <MobileTopBar onOpenMenu={() => setDrawerOpen(true)} />
        <main className="shell-content shell-content--mobile">
          <Outlet />
        </main>
        <MobileNavDrawer open={drawerOpen} onClose={() => setDrawerOpen(false)} />
        <TroubleshootToast />
      </div>
    );
  }

  return (
    <div className="shell">
      <TopBar />
      <div className="shell-body">
        <SideBar />
        <main className="shell-content">
          <Outlet />
        </main>
      </div>
      <TroubleshootToast />
    </div>
  );
}
