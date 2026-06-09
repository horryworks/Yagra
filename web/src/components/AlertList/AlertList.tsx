// Alert-list pane: live alerts worst-first, color-coded by severity.

import { severityColorVar } from '../../lib/format';
import { sortedAlerts, useAlertStore } from '../../store';
import { formatTimestamp } from '../../lib/format';

export function AlertList() {
  const alerts = useAlertStore((s) => s.alerts);
  const list = sortedAlerts(alerts);

  return (
    <section className="pane">
      <h2>Alerts ({list.length})</h2>
      {list.length === 0 ? (
        <p className="muted">No active alerts.</p>
      ) : (
        list.map((a) => (
          <div className="alert-row" key={`${a.node}|${a.check}|${a.severity}`}>
            <span
              className="alert-dot"
              style={{ background: severityColorVar(a.severity) }}
            />
            <span style={{ flex: 1 }}>
              {a.node}
              {a.root_cause && <span className="muted"> ← {a.root_cause}</span>}
            </span>
            {a.flapping && <span className="alert-flapping">flapping</span>}
            <span className="muted">{formatTimestamp(a.at_unix_ms)}</span>
          </div>
        ))
      )}
    </section>
  );
}
