// 02 · Alerts widgets. Active alerts + flapping + severity mix read the live alert store (the
// page subscribes once via useAlertStream); alert volume buckets the history endpoint client-side.

import { useNavigate } from 'react-router-dom';
import { Button } from '../../components/ui/Button';
import { relativeTime, severityColorVar, stateColorVar, stateLabel } from '../../lib/format';
import { api } from '../../services/api';
import { sortedAlerts, useAlertStore } from '../../store';
import { AlertRows } from '../../widgets/AlertRows';
import { Donut } from '../primitives/Donut';
import { Heatmap } from '../primitives/Heatmap';
import { RankedBars } from '../primitives/RankedBars';
import { usePolled } from '../usePolled';
import { bucketAlertsByHour, calendarMatrix } from './util';

const DOW_LABELS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const HOUR_LABELS = Array.from({ length: 24 }, (_, h) => (h % 6 === 0 ? String(h) : ''));

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
  const { data, loading, error } = usePolled(() => api.listAlertHistory({ limit: 1000 }), []);
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

export function TopAlertingNodesWidget() {
  const { data, loading, error } = usePolled(() => api.getAlertTopNodes({ limit: 6 }), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">Loading…</p>;
  const rows = (data ?? []).map((e) => ({
    label: e.name,
    value: e.count,
    valueText: String(e.count),
    color: 'var(--series-2)',
  }));
  return <RankedBars rows={rows} empty="No alerts recorded yet." />;
}

export function AlertCalendarWidget() {
  const { data, loading, error } = usePolled(() => api.getAlertCalendar(7), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">Loading…</p>;
  const matrix = calendarMatrix(data ?? []);
  if ((data ?? []).length === 0) return <p className="muted">No alerts in the last 7 days.</p>;
  return (
    <Heatmap
      rowLabels={DOW_LABELS}
      colLabels={HOUR_LABELS}
      values={matrix}
      colorBase="var(--series-2)"
      title={(row, col, v) => `${row} ${col || ''}:00 — ${v} alert${v === 1 ? '' : 's'}`}
    />
  );
}

export function RecentStateChangesWidget() {
  const { data, loading, error } = usePolled(() => api.getAlertTransitions(12), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">Loading…</p>;
  if ((data ?? []).length === 0) return <p className="muted">No recent state changes.</p>;
  return (
    <ul className="dwl">
      {(data ?? []).map((t, i) => (
        <li className="dwl-row" key={`${t.node_id}-${t.at_unix_ms}-${i}`}>
          <span
            className="dwl-dot"
            style={{ background: t.resolved ? stateColorVar('ok') : severityColorVar(t.severity) }}
          />
          <span className="dwl-name">{t.name}</span>
          <span className="dwl-sub muted">
            {t.resolved ? '→ recovered' : `→ ${stateLabel(t.state)}`} ·{' '}
            {relativeTime(new Date(t.at_unix_ms).toISOString(), Date.now())}
          </span>
        </li>
      ))}
    </ul>
  );
}
