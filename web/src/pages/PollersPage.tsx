// Pollers & system health (Settings ▸ Pollers). Yagra's own health in one place: the poll loop's
// live counters, backing-service reachability (PostgreSQL / TSDB / bus), and fleet data coverage.
// Read-only; the backend gates each read with View. Reuses the dashboard monitoring widgets so this
// page and the dashboard cards stay in sync.

import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Badge } from '../components/ui/Badge';
import { api } from '../services/api';
import { usePolled } from '../dashboard/usePolled';
import { PollerHealthWidget, DataCoverageWidget } from '../dashboard/widgets/monitoring';
import type { DependencyHealth } from '../types/api';
import './PollersPage.css';

/** One backing dependency as a name + detail + reachability badge. */
function DependencyRow({ name, dep }: { name: string; dep: DependencyHealth | undefined }) {
  return (
    <li className="dep-row">
      <span className="dep-name">{name}</span>
      <span className="dep-detail muted">{dep?.detail ?? '—'}</span>
      <Badge tone={dep?.reachable ? 'up' : 'critical'}>
        {dep?.reachable ? 'Reachable' : 'Unreachable'}
      </Badge>
    </li>
  );
}

/** Backing-service reachability (PostgreSQL / TSDB / bus). Bus is an indirect signal inferred from
 *  poll-loop activity — the row detail spells that out. */
function DependencyHealthCard() {
  const { data, loading, error } = usePolled(() => api.getSystemHealth(), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">Loading…</p>;
  return (
    <ul className="dep-list">
      <DependencyRow name="PostgreSQL" dep={data?.postgres} />
      <DependencyRow name="VictoriaMetrics (TSDB)" dep={data?.tsdb} />
      <DependencyRow name="NATS bus" dep={data?.bus} />
    </ul>
  );
}

export function PollersPage() {
  return (
    <div>
      <PageHeader
        title="Pollers & system health"
        trail={[{ label: 'Settings' }, { label: 'Pollers' }]}
        note="Yagra's own health: the poll loop, its backing services, and fleet data coverage."
      />
      <div className="pollers-grid">
        <Card title="Poll-loop health">
          <PollerHealthWidget />
        </Card>
        <Card title="Dependency health">
          <DependencyHealthCard />
        </Card>
        <Card title="Data coverage">
          <DataCoverageWidget />
        </Card>
      </div>
    </div>
  );
}
