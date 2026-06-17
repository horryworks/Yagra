// 02 · Alerts widgets. Active alerts + flapping + severity mix read the live alert store (the
// page subscribes once via useAlertStream); alert volume buckets the history endpoint client-side.

import { useNavigate } from 'react-router-dom';
import { Button } from '../../components/ui/Button';
import { severityColorVar } from '../../lib/format';
import { api } from '../../services/api';
import { sortedAlerts, useAlertStore } from '../../store';
import { AlertRows } from '../../widgets/AlertRows';
import { Donut } from '../primitives/Donut';
import { usePolled } from '../usePolled';
import { bucketAlertsByHour } from './util';

export function ActiveAlertsWidget() {
  return <AlertRows limit={8} empty="No active alerts. All monitored nodes are healthy." />;
}

/** View-mode header action for the active-alerts widget: jump to the full triage screen. */
export function ActiveAlertsActions() {
  const navigate = useNavigate();
  return (
    <Button variant="ghost" onClick={() => navigate('/alerts')}>
      View all
    </Button>
  );
}

export function SeverityMixWidget() {
  const alerts = useAlertStore((s) => s.alerts);
  const list = Object.values(alerts);
  if (list.length === 0) return <p className="muted">No active alerts.</p>;
  const by = { critical: 0, warning: 0, info: 0 };
  for (const a of list) by[a.severity] += 1;
  const segments = [
    { label: 'Critical', value: by.critical, color: severityColorVar('critical') },
    { label: 'Warning', value: by.warning, color: severityColorVar('warning') },
    { label: 'Info', value: by.info, color: severityColorVar('info') },
  ].filter((s) => s.value > 0);
  return <Donut segments={segments} centerValue={String(list.length)} centerSub="active" />;
}

export function FlappingWatchlistWidget() {
  const alerts = useAlertStore((s) => s.alerts);
  const flapping = sortedAlerts(alerts).filter((a) => a.flapping);
  if (flapping.length === 0) {
    return <p className="muted">No flapping checks. State is stable.</p>;
  }
  return (
    <ul className="dwl">
      {flapping.map((a) => (
        <li className="dwl-row" key={`${a.node}|${a.check}|${a.severity}`}>
          <span className="dwl-dot" style={{ background: severityColorVar(a.severity) }} />
          <span className="dwl-name mono">{a.node}</span>
          <span className="dwl-sub">{a.check}</span>
        </li>
      ))}
    </ul>
  );
}

export function AlertVolumeWidget() {
  const { data, loading, error } = usePolled(() => api.listAlertHistory(1000), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">Loading…</p>;
  const buckets = bucketAlertsByHour(data ?? [], 24, Date.now());
  const total = buckets.reduce((n, b) => n + b.count, 0);
  if (total === 0) return <p className="muted">No alerts opened in the last 24h.</p>;
  const peak = Math.max(...buckets.map((b) => b.count), 1);
  return (
    <div className="vbars" role="img" aria-label="alerts opened per hour over the last 24 hours">
      {buckets.map((b) => (
        <span className="vbar" key={b.t} title={`${b.count} opened`}>
          <span className="vbar-fill" style={{ height: `${(b.count / peak) * 100}%` }} />
        </span>
      ))}
    </div>
  );
}
