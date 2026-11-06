// SPDX-License-Identifier: AGPL-3.0-only
// The notes beside an active alert — its cause, flapping, and the two holds (ADR-243 decision 5).
// Which notes a row carries is `lib/alertNotes.ts`; this file only draws them. The Active alerts
// table stacks them one per line; the dashboard widget's compact row lays them side by side.

import { Fragment } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { EntityName } from '../components/ui/EntityName';
import { noteKey, type AlertNote } from '../lib/alertNotes';
import { nodeHref } from '../lib/entityHref';
import { formatTimestamp } from '../lib/format';
import './AlertNotes.css';

export function AlertNotes({
  notes,
  nodeName,
  stacked = false,
}: {
  notes: readonly AlertNote[];
  nodeName: (id: string) => string;
  /** One note per line (the table's Notes column), rather than side by side (the widget's row). */
  stacked?: boolean;
}) {
  const { t } = useTranslation('alerts');
  const items = notes.map((n) => <Fragment key={noteKey(n)}>{draw(n)}</Fragment>);
  return stacked ? <span className="alert-notes">{items}</span> : <>{items}</>;

  function draw(n: AlertNote) {
    switch (n.kind) {
      case 'upstream':
        // Another node's outage is the cause (ADR-236). The name opens that node.
        // The whole note in `title`, for when the stacked column cuts a long name.
        return (
          <span className="alertrow-cause muted" title={`← ${nodeName(n.nodeId)}`}>
            ← <EntityName name={nodeName(n.nodeId)} id={n.nodeId} to={nodeHref(n.nodeId)} />
          </span>
        );
      case 'self':
        return (
          <span className="alertrow-cause muted" title={t('row.partOfOutage')}>
            {t('row.partOfOutage')}
          </span>
        );
      case 'flapping':
        return (
          <span className="alertrow-flap" title={t('row.flapping')}>
            {t('row.flapping')}
          </span>
        );
      case 'waitingNeighbour':
        // Shown, not paged yet: a linked neighbour is failing and may be the real cause (ADR-236
        // Inc.2). Not the delivery-hours hold below, which is a different wait.
        return (
          <span className="alertrow-muted" title={t('row.waitingNeighbour')}>
            {t('row.waitingNeighbour')}
          </span>
        );
      case 'held': {
        // Outside its node's delivery hours the page waits (ADR-226 decision 11); the mark opens
        // the screen where the hours are kept.
        const text = n.releaseAt
          ? t('row.heldUntil', { until: formatTimestamp(Date.parse(n.releaseAt)) })
          : t('row.held');
        return (
          <Link className="alertrow-muted" to="/alerts/routing" title={text}>
            {text}
          </Link>
        );
      }
      default: {
        const unknown: never = n;
        return unknown;
      }
    }
  }
}
