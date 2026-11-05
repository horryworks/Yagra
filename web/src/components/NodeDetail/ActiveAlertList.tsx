// SPDX-License-Identifier: AGPL-3.0-only
// One node's active alerts, as rows: Overview's Active alerts section and the list the header's
// state badge opens both draw them here, so the two cannot say different things (ADR-238 decision 2).

import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { rootCause, type HasSubject } from '../../lib/alertSubject';
import { alertWhatOf, formatTimestamp, severityColorVar, stateLabel } from '../../lib/format';
import type { NodeStatus } from '../../types/api';
import { AlertWhatText } from '../../widgets/AlertWhatText';
import { EntityName } from '../ui/EntityName';
import { useEntityNames } from '../ui/entityNames';

export type NodeAlert = NodeStatus['alerts'][number];

export function ActiveAlertList({
  alerts,
  action,
}: {
  alerts: readonly NodeAlert[];
  /** Drawn after the row's text, before its time: the header list's jump button. */
  action?: (alert: NodeAlert) => ReactNode;
}) {
  const { t } = useTranslation('nodes');
  return (
    <div className="nd-alerts">
      {alerts.map((a) => (
        <div className="nd-alert" key={`${a.check}|${a.severity}`}>
          <span className="nd-alert-dot" style={{ background: severityColorVar(a.severity) }} />
          <span className="nd-alert-state">{stateLabel(a.state)}</span>
          {/* WHY it fired - the metric, the bound it crossed and the sample that crossed it. The
              same span, on the same fact, as Alerts > Active and Alerts > History. `title` is the
              check id, matching the triage row; the words carry their own tooltip from inside
              AlertWhatText. */}
          <span className="nd-alert-what" title={a.check}>
            <AlertWhatText what={alertWhatOf(a)} />
          </span>
          {/* `causedBy` used to interpolate the raw UUID, which `no-raw-uuids-in-tables` forbids
              and which ADR-087 made worse: on this very node's page the cause is often the node
              itself, so it read "caused by <this node's own uuid>". */}
          <AlertCause alert={a} />
          {a.flapping && <span className="nd-alert-flap">{t('alerts:row.flapping')}</span>}
          {action?.(a)}
          <span className="nd-muted nd-alert-time">{formatTimestamp(a.at_unix_ms)}</span>
        </div>
      ))}
    </div>
  );
}

/** How an alert's cause reads on the node's own page (ADR-087).
 *
 * `self` is the common case here and the one that has no other node to name: this page *is* the
 * node whose outage the alert belongs to. `upstream` resolves the id to a name — the old copy
 * interpolated the raw UUID, which `no-raw-uuids-in-tables` forbids. */
function AlertCause({ alert }: { alert: HasSubject & { root_cause?: string | null } }) {
  const { t } = useTranslation('nodes');
  // Hooks before the early returns: this component renders nothing for most alerts, and a hook
  // after a conditional return is the classic React rules-of-hooks break.
  const { nodeName } = useEntityNames();
  const cause = rootCause(alert);
  if (cause.kind === 'none') return null;
  if (cause.kind === 'self') {
    return <span className="nd-muted nd-alert-cause">{t('overview.partOfOutage')}</span>;
  }
  return (
    <span className="nd-muted nd-alert-cause">
      ← <EntityName name={nodeName(cause.nodeId)} id={cause.nodeId} />
    </span>
  );
}
