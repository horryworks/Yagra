// SPDX-License-Identifier: AGPL-3.0-only
// "Monitored at another address as …" — the nodes a discovered device looks like (ADR-139 Inc.3),
// with the evidence for each and a line saying what pressing on does. Drawn by the Scan tab's
// candidate rows and, after a Detect, by the Monitoring setup cell that Discovery ▸ Unregistered
// devices and Node ▸ Neighbors share (Inc.4). One component, so the two cannot word it differently.
//
// Rendering only: which nodes, and the keys for the badge and the reasons, are decided in
// `pages/discoveryExisting.ts`, where a test reaches them.

import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import type { SameDeviceMatch } from '../../types/api';
import { sameDeviceBadgeKey, sameDeviceReasonKey } from '../../pages/discoveryExisting';
import { nodeHref } from '../../lib/entityHref';
import { Badge } from '../ui/Badge';
import './SameDeviceNote.css';

interface Props {
  match: SameDeviceMatch;
  /** What the operator can still do, in the surface's own terms — a tick box on one, a button on
   *  the other. Shown in the row, never only as a `title=` (ADR-055 R4). */
  hint: string;
  /** Draw the badge at the start. The Scan tab puts it beside the address instead. */
  badge?: boolean;
}

export function SameDeviceNote({ match, hint, badge = false }: Props) {
  const { t } = useTranslation('monitoring');
  // Wraps rather than truncating: the evidence and the hint are the whole point of the mark.
  return (
    <span className="same-note">
      {badge && (
        // Neutral: a status tone would claim a monitoring state.
        <>
          <Badge tone="neutral">{t(sameDeviceBadgeKey(match))}</Badge>{' '}
        </>
      )}
      <span className="muted">{t('discovery.sameDevice.as')}</span>{' '}
      {match.nodes.map((n, i) => {
        const why = n.evidence
          .map(sameDeviceReasonKey)
          .filter((k): k is string => k !== null)
          .map((k) => t(k))
          .join(' · ');
        return (
          <span key={n.id}>
            {i > 0 && ', '}
            <Link to={nodeHref(n.id)}>{n.name}</Link>{' '}
            <span className="mono muted">({n.address})</span>
            {why && <span className="muted same-note-small"> {why}</span>}
          </span>
        );
      })}
      <span className="muted same-note-small same-note-hint">{hint}</span>
    </span>
  );
}
