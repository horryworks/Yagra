// Big-number KPI tile: a headline value + unit, a caption, and an optional delta vs a prior
// window. The delta is categorical, not a node status, so its ▲/▼ uses neutral series-ish text
// tones (never status red/green) — direction is shown by the arrow + sign, not color alone.

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
}

export function KpiTile({ value, caption, delta, deltaLabel }: Props) {
  const dir = delta == null ? null : delta > 0 ? 'up' : delta < 0 ? 'down' : 'flat';
  return (
    <div className="kpitile">
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
    </div>
  );
}
