// App shell: top bar + collapsible sidebar + routed content (§2 IA). The content region owns
// its own scroll so the chrome stays fixed and live lists/tables scroll within their pane.

import { Outlet } from 'react-router-dom';
import { SideBar } from './SideBar';
import { TopBar } from './TopBar';
import './AppShell.css';

export function AppShell() {
  return (
    <div className="shell">
      <TopBar />
      <div className="shell-body">
        <SideBar />
        <main className="shell-content">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
