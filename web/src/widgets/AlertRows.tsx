// Presentational alert rows (worst-first), shared by the dashboard widget and the Active
// alerts triage screen. Each row: severity dot + node + root-cause + flapping flag + age.
// Triage-only per §3.2 — NO Ack control here (Yagra holds no ack/escalation; that's external
// PagerDuty/JSM). The optional actions slot is for Mute / "open in external tool" only.

import type { ReactNode } from 'react';
import { formatTimestamp, severityColorVar } from '../lib/format';
import { sortedAlerts, useAlertStore } from '../store';
import './AlertRows.css';

interface Props {
  /** Cap the number of rows (e.g. dashboard widget). */
  limit?: number;
  /** Per-row actions (Mute / open external). Triage-only — never an Ack. */
  actions?: (alert: ReturnType<typeof sortedAlerts>[number]) => ReactNode;
  empty?: ReactNode;
}

export function AlertRows({ limit, actions, empty }: Props) {
  const alerts = useAlertStore((s) => s.alerts);
  const all = sortedAlerts(alerts);
  const list = limit ? all.slice(0, limit) : all;

  if (all.length === 0) {
    return <p className="alertrows-empty">{empty ?? 'No active alerts.'}</p>;
  }

  return (
    <div className="alertrows">
      {list.map((a) => (
        <div className="alertrow" key={`${a.node}|${a.check}|${a.severity}`}>
          <span className="alertrow-dot" style={{ background: severityColorVar(a.severity) }} />
          <span className="alertrow-node mono">{a.node}</span>
          <span className="alertrow-check">{a.check}</span>
          {a.root_cause && (
            <span className="alertrow-cause muted">← {a.root_cause}</span>
          )}
          {a.flapping && <span className="alertrow-flap">flapping</span>}
          <span className="alertrow-time muted">{formatTimestamp(a.at_unix_ms)}</span>
          {actions && <span className="alertrow-actions">{actions(a)}</span>}
        </div>
      ))}
    </div>
  );
}
