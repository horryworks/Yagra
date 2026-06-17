// 270° arc gauge with a center percentage. Powers the data-coverage widget. The arc color is
// passed in (the widget owns the channel — status for coverage). Value is clamped to 0–100.

import './Gauge.css';

interface Props {
  /** 0–100. */
  value: number;
  /** Small caption under the big number (e.g. "% fresh"). */
  centerLabel?: string;
  /** Extra sub-line under the gauge (e.g. "42/50 nodes"). */
  sub?: string;
  /** Arc color (CSS var); default series-1. */
  color?: string;
}

// r chosen so the full circumference is 100; the gauge uses 270° = 75 of those units, leaving a
// 90° gap at the bottom (rotate -135° so the gap is centered there).
const R = 15.915;
const SWEEP = 75; // 270°

export function Gauge({ value, centerLabel, sub, color = 'var(--series-1)' }: Props) {
  const pct = Math.max(0, Math.min(100, value));
  const filled = (pct / 100) * SWEEP;
  return (
    <div className="gauge">
      <svg className="gauge-svg" viewBox="0 0 42 42" role="img" aria-label={`${Math.round(pct)}%`}>
        <g transform="rotate(135 21 21)">
          <circle
            className="gauge-track"
            cx="21"
            cy="21"
            r={R}
            strokeDasharray={`${SWEEP} ${100 - SWEEP}`}
          />
          <circle
            className="gauge-fill"
            cx="21"
            cy="21"
            r={R}
            stroke={color}
            strokeDasharray={`${filled} ${100 - filled}`}
          />
        </g>
        <text className="gauge-value" x="21" y="20" textAnchor="middle">
          {Math.round(pct)}
        </text>
        {centerLabel && (
          <text className="gauge-label" x="21" y="25" textAnchor="middle">
            {centerLabel}
          </text>
        )}
      </svg>
      {sub && <div className="gauge-sub">{sub}</div>}
    </div>
  );
}
