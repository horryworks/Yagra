// 06 · Monitoring-health widgets. Maintenance windows (what's suppressed now + upcoming) and a
// recent-changes feed off the audit log (admin-only — non-admins see a friendly gate). Both are
// straight reads of existing endpoints.

import { useTranslation } from 'react-i18next';
import { Badge } from '../../components/ui/Badge';
import {
  formatCount,
  httpStatusLabel,
  httpStatusTone,
  relativeTime,
  stateColorVar,
} from '../../lib/format';
import { api } from '../../services/api';
import { Gauge } from '../primitives/Gauge';
import { usePolled } from '../usePolled';

export function MaintenanceWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.listMaintenanceWindows(), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const now = Date.now();
  const windows = (data ?? [])
    .filter((w) => w.active || (w.enabled && new Date(w.starts_at).getTime() > now))
    // Active first, then soonest-starting upcoming.
    .sort((a, b) => Number(b.active) - Number(a.active) || a.starts_at.localeCompare(b.starts_at))
    .slice(0, 6);
  if (windows.length === 0) {
    return <p className="muted">{t('widgets.maintenance.empty')}</p>;
  }
  return (
    <ul className="dwl">
      {windows.map((w) => (
        <li className="dwl-row" key={w.id}>
          <Badge tone={w.active ? 'info' : 'neutral'}>
            {w.active ? t('widgets.maintenance.active') : t('widgets.maintenance.upcoming')}
          </Badge>
          <span className="dwl-name">{w.name}</span>
          <span className="dwl-sub muted">
            {w.active
              ? t('widgets.maintenance.ends', { time: relativeTime(w.ends_at, now) })
              : relativeTime(w.starts_at, now)}
          </span>
        </li>
      ))}
    </ul>
  );
}

export function DataCoverageWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getFleetCoverage(), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const pct = data?.coverage_pct ?? 0;
  // Coverage reads as health: high = good. Threshold onto the status channel.
  const color =
    pct >= 95 ? stateColorVar('ok') : pct >= 80 ? stateColorVar('warning') : stateColorVar('critical');
  return (
    <Gauge
      value={pct}
      centerLabel={t('widgets.dataCoverage.freshPct')}
      color={color}
      sub={t('widgets.dataCoverage.nodes', { fresh: data?.fresh ?? 0, total: data?.total ?? 0 })}
    />
  );
}

export function StaleDataWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getFleetCoverage(), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const stale = data?.stale ?? [];
  if (stale.length === 0) return <p className="muted">{t('widgets.staleData.empty')}</p>;
  return (
    <ul className="dwl">
      {stale.map((s) => (
        <li className="dwl-row" key={s.node_id}>
          <span className="dwl-dot" style={{ background: stateColorVar('unknown') }} />
          <span className="dwl-name">{s.name}</span>
          <span className="dwl-sub muted">{t('widgets.staleData.noRecent')}</span>
        </li>
      ))}
    </ul>
  );
}

export function PollerHealthWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getPollerHealth(), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const lastSweep =
    data?.last_sweep_unix_ms != null
      ? relativeTime(new Date(data.last_sweep_unix_ms).toISOString(), Date.now())
      : t('widgets.pollerHealth.never');
  return (
    <div className="statstrip">
      <div className="statstrip-item">
        <span className="statstrip-val">{lastSweep}</span>
        <span className="statstrip-cap">{t('widgets.pollerHealth.lastRun')}</span>
      </div>
      <div className="statstrip-item">
        <span className="statstrip-val">{formatCount(data?.jobs_last_round ?? 0)}</span>
        <span className="statstrip-cap">{t('widgets.pollerHealth.jobsPerRun')}</span>
      </div>
      <div className="statstrip-item">
        <span className="statstrip-val">{formatCount(data?.results_total ?? 0)}</span>
        <span className="statstrip-cap">{t('widgets.pollerHealth.results')}</span>
      </div>
    </div>
  );
}

export function DiscoveryQueueWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getDiscoveryCandidates(10), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const candidates = data ?? [];
  if (candidates.length === 0) {
    return <p className="muted">{t('widgets.discovery.empty')}</p>;
  }
  return (
    <ul className="dwl">
      {candidates.map((c) => (
        <li className="dwl-row" key={c.address}>
          <span className="dwl-name mono">{c.address}</span>
          <span className="dwl-sub muted">
            {c.sysname ?? c.sysdescr ?? t('widgets.discovery.unclassified')}
          </span>
        </li>
      ))}
    </ul>
  );
}

export function AuditWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.listAudit({ limit: 8 }), []);
  // The audit log is admin-only; a viewer/operator gets a 403 — show a gate, not a raw error.
  if (error) {
    return <p className="muted">{t('widgets.audit.forbidden')}</p>;
  }
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  if ((data ?? []).length === 0) return <p className="muted">{t('widgets.audit.empty')}</p>;
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
