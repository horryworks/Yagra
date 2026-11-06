// SPDX-License-Identifier: AGPL-3.0-only
// Presentational alert rows (worst-first) for the dashboard's Active alerts widget. Each row:
// severity dot + node name + what fired + its notes + age. Triage-only per §3.2 — Yagra does not
// track acknowledgement at all (ADR-233); that belongs to the external on-call tool.
//
// The full Alerts ▸ Active screen does not use this since ADR-243: it is the shared `DataTable`
// with History's columns. What the two share is the part that decides content — which notes a row
// carries (`lib/alertNotes.ts`, drawn by `AlertNotes`) and how the subject and the check read —
// so the widget and the screen cannot disagree about an alert, only about how much room it gets.
//
// Two things the row deliberately does NOT show as raw ids. The node and its root cause resolve to
// names via the shared `useEntityNames` (the visible primary is always the name). The check id has
// no name to resolve to — it is a one-way hash of (node, metric), which is exactly why the metric
// is persisted alongside the alert (migration 0036) — so the row shows what the check *measured*.
//
// The widget is capped (`limit`), a small fixed slice, so it renders inline and is not windowed.

import { useCallback, useMemo } from 'react';
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { subjectNodeId, alertRowKey as rowKey } from '../lib/alertSubject';
import { alertNotes } from '../lib/alertNotes';
import { alertWhatOf, formatTimestamp, severityColorVar } from '../lib/format';
import { sortedAlerts, useAlertStore } from '../store';
import { useHeldNotifications } from './useHeldNotifications';
import { heldFireFor } from '../pages/deliveryHours';
import { useEntityNames } from '../components/ui/entityNames';
import type { Alert } from '../types/api';
import { AlertSubjectName } from './AlertSubjectName';
import { AlertWhatText } from './AlertWhatText';
import { AlertNotes } from './AlertNotes';
import './AlertRows.css';

interface Props {
  /** How many rows the widget shows. */
  limit: number;
  empty?: ReactNode;
}

export function AlertRows({ limit, empty }: Props) {
  const { t } = useTranslation('alerts');
  const alerts = useAlertStore((s) => s.alerts);
  // One resolver for the whole list: `nodeName` enqueues only the ids this render referenced, and
  // batch-resolves them after the commit.
  const { nodeName } = useEntityNames();
  const held = useHeldNotifications();
  const heldOf = useCallback(
    (a: Alert) => (held.length === 0 ? null : heldFireFor(a, held, subjectNodeId(a))),
    [held],
  );
  // Sort once per store change, not on every SSE-driven render.
  const shown = useMemo(() => sortedAlerts(alerts).slice(0, limit), [alerts, limit]);

  if (shown.length === 0) {
    return <p className="alertrows-empty">{empty ?? t('active.empty')}</p>;
  }
  return (
    <div className="alertrows">
      {shown.map((a) => (
        <div key={rowKey(a)} className="alertrow">
          <span className="alertrow-dot" style={{ background: severityColorVar(a.severity) }} />
          <span className="alertrow-node">
            <AlertSubjectName alert={a} nodeName={nodeName} />
          </span>
          {/* The check id is still the handle the API takes (RCA), so keep it recoverable on
              hover even though it is no longer the cell's text. */}
          <span className="alertrow-check" title={a.check}>
            <AlertWhatText what={alertWhatOf(a)} />
          </span>
          <AlertNotes notes={alertNotes(a, heldOf(a))} nodeName={nodeName} />
          <span className="alertrow-time muted">{formatTimestamp(a.at_unix_ms)}</span>
        </div>
      ))}
    </div>
  );
}
