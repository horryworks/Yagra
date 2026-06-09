import { useEffect, useState } from 'react';
import { Admin } from './components/Admin/Admin';
import { Dashboard } from './components/Dashboard/Dashboard';
import { Login } from './components/Login/Login';
import { api, type ClientConfig } from './services/api';
import { useAuthStore } from './store';

export function App() {
  const authed = useAuthStore((s) => s.authed);
  const [config, setConfig] = useState<ClientConfig | null>(null);

  // Discover whether the server gates reads behind login. On error, fall back to an open
  // dashboard so a transient/older server never hard-locks the UI.
  useEffect(() => {
    api
      .getConfig()
      .then(setConfig)
      .catch(() => setConfig({ public_dashboard: true, auth_available: false }));
  }, []);

  // Private mode (reads require auth) and not logged in ⇒ gate the whole app behind login.
  const gated = config != null && !config.public_dashboard && !authed;

  return (
    <>
      <header className="app-header">
        <h1>Yagra</h1>
        <span className="muted">Network Monitoring</span>
      </header>
      {config == null ? (
        <main className="app-main">
          <p className="muted">Loading…</p>
        </main>
      ) : gated ? (
        <main className="app-main">
          <Login title="Sign in to Yagra" />
        </main>
      ) : (
        <>
          <Dashboard />
          <main className="app-main">
            <Admin />
          </main>
        </>
      )}
    </>
  );
}
