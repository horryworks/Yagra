import { Admin } from './components/Admin/Admin';
import { Dashboard } from './components/Dashboard/Dashboard';

export function App() {
  return (
    <>
      <header className="app-header">
        <h1>Yagra</h1>
        <span className="muted">Network Monitoring</span>
      </header>
      <Dashboard />
      <main className="app-main">
        <Admin />
      </main>
    </>
  );
}
