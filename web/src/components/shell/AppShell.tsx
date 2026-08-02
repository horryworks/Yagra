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

import { useEffect, useState } from 'react';
import { Outlet, useLocation } from 'react-router-dom';
import { SideBar } from './SideBar';
import { TopBar } from './TopBar';
import { MobileTopBar } from './MobileTopBar';
import { MobileNavDrawer } from './MobileNavDrawer';
import { useViewportMode } from '../../lib/viewport';
import { useTroubleshootStream } from '../../troubleshoot/useTroubleshootStream';
import { TroubleshootToast } from '../../troubleshoot/TroubleshootToast';
import './AppShell.css';

export function AppShell() {
  const mobile = useViewportMode() === 'mobile';
  const [drawerOpen, setDrawerOpen] = useState(false);
  const { pathname } = useLocation();
  useTroubleshootStream();

  // Close the drawer on any route change — covers the browser Back button and a tap on the
  // already-current route (which wouldn't change the pathname otherwise).
  useEffect(() => {
    setDrawerOpen(false);
  }, [pathname]);

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
