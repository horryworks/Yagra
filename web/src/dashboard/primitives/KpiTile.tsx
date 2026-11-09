// SPDX-License-Identifier: AGPL-3.0-only
// Big-number KPI tile: a headline value + unit, a caption, and an optional delta vs a prior
// window. The delta is categorical, not a node status, so its ▲/▼ uses neutral series-ish text
// tones (never status red/green) — direction is shown by the arrow + sign, not color alone.

import { DrillLink } from '../../components/ui/DrillLink';
import './KpiTile.css';

interface Props {
  /** Headline value (already formatted, e.g. "3" or "128"). */
  value: string;
  /** Small unit/caption under the value (e.g. "nodes down"). */
  caption: string;
  /** Optional change vs a prior window; sign drives the arrow. 0 ⇒ "no change". */
  delta?: number;
  /** Word describing the delta window (e.g. "vs 1h ago"). */
  deltaLabel?: string;
  /** The screen listing what the figure counts (ADR-250). The whole tile becomes the link. */
  to?: string;
}

export function KpiTile({ value, caption, delta, deltaLabel, to }: Props) {
  const dir = delta == null ? null : delta > 0 ? 'up' : delta < 0 ? 'down' : 'flat';
  return (
    <DrillLink to={to} block className="kpitile">
      <div className="kpitile-value">{value}</div>
      <div className="kpitile-caption">{caption}</div>
      {dir && (
        <div className={`kpitile-delta kpitile-delta-${dir}`}>
          <span className="kpitile-delta-arrow" aria-hidden="true">
            {dir === 'up' ? '▲' : dir === 'down' ? '▼' : '◆'}
          </span>
          <span className="kpitile-delta-num">
            {delta! > 0 ? '+' : ''}
            {delta}
          </span>
          {deltaLabel && <span className="kpitile-delta-label">{deltaLabel}</span>}
        </div>
      )}
    </DrillLink>
  );
}
