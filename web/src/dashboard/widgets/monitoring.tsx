// 06 · Monitoring-health widgets. Maintenance windows (what's suppressed now + upcoming) and a
// recent-changes feed off the audit log (admin-only — non-admins see a friendly gate). Both are
// straight reads of existing endpoints.

import { Badge } from '../../components/ui/Badge';
import { httpStatusLabel, httpStatusTone, relativeTime } from '../../lib/format';
import { api } from '../../services/api';
import { usePolled } from '../usePolled';

export function MaintenanceWidget() {
  const { data, loading, error } = usePolled(() => api.listMaintenanceWindows(), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">Loading…</p>;
  const now = Date.now();
  const windows = (data ?? [])
    .filter((w) => w.active || (w.enabled && new Date(w.starts_at).getTime() > now))
    // Active first, then soonest-starting upcoming.
    .sort((a, b) => Number(b.active) - Number(a.active) || a.starts_at.localeCompare(b.starts_at))
    .slice(0, 6);
  if (windows.length === 0) {
    return <p className="muted">Nothing in maintenance now or scheduled.</p>;
  }
  return (
    <ul className="dwl">
      {windows.map((w) => (
        <li className="dwl-row" key={w.id}>
          <Badge tone={w.active ? 'info' : 'neutral'}>{w.active ? 'Active' : 'Upcoming'}</Badge>
          <span className="dwl-name">{w.name}</span>
          <span className="dwl-sub muted">
            {w.active ? `ends ${relativeTime(w.ends_at, now)}` : relativeTime(w.starts_at, now)}
          </span>
        </li>
      ))}
    </ul>
  );
}

export function AuditWidget() {
  const { data, loading, error } = usePolled(() => api.listAudit({ limit: 8 }), []);
  // The audit log is admin-only; a viewer/operator gets a 403 — show a gate, not a raw error.
  if (error) {
    return <p className="muted">Recent changes require an admin role to view.</p>;
  }
  if (loading && !data) return <p className="muted">Loading…</p>;
  if ((data ?? []).length === 0) return <p className="muted">No recent changes.</p>;
  return (
    <ul className="dwl">
      {(data ?? []).map((row) => (
        <li className="dwl-row" key={row.id}>
          <Badge tone={httpStatusTone(row.status)}>{httpStatusLabel(row.status)}</Badge>
          <span className="dwl-name mono">{row.action}</span>
          <span className="dwl-sub muted">
            {row.username} · {relativeTime(row.at, Date.now())}
          </span>
        </li>
      ))}
    </ul>
  );
}
