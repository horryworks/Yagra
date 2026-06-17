// Segmented donut ring with an optional center readout + legend. Powers the health ring and
// severity mix widgets. Colors are passed in (CSS-var strings) — the widget chooses the channel
// (status for health, severity for the alert mix), so this primitive stays channel-agnostic.

import './Donut.css';

export interface DonutSegment {
  label: string;
  value: number;
  /** CSS color (e.g. `var(--status-up)`). */
  color: string;
}

interface Props {
  segments: DonutSegment[];
  /** Big number in the hole (e.g. "88"). */
  centerValue?: string;
  /** Small caption under the center value (e.g. "% healthy"). */
  centerSub?: string;
  /** Show the label · value legend beside the ring (default true). */
  legend?: boolean;
}

// r chosen so the circumference is ~100 ⇒ dasharray values read as percentages directly.
const R = 15.915;
const CIRC = 2 * Math.PI * R;

export function Donut({ segments, centerValue, centerSub, legend = true }: Props) {
  const total = segments.reduce((s, x) => s + Math.max(0, x.value), 0);
  // Cumulative offset so segments lay end-to-end starting at 12 o'clock.
  let acc = 0;
  return (
    <div className="donut">
      <svg className="donut-svg" viewBox="0 0 42 42" role="img" aria-label="distribution">
        <circle className="donut-track" cx="21" cy="21" r={R} />
        {total > 0 &&
          segments.map((seg) => {
            const frac = Math.max(0, seg.value) / total;
            const len = frac * CIRC;
            const dash = `${len} ${CIRC - len}`;
            // Start at top (25% of the circle) and walk backwards by the accumulated length.
            const offset = CIRC * 0.25 - acc;
            acc += len;
            return (
              <circle
                key={seg.label}
                className="donut-seg"
                cx="21"
                cy="21"
                r={R}
                stroke={seg.color}
                strokeDasharray={dash}
                strokeDashoffset={offset}
              />
            );
          })}
        {centerValue != null && (
          <text className="donut-center-value" x="21" y="20" textAnchor="middle">
            {centerValue}
          </text>
        )}
        {centerSub != null && (
          <text className="donut-center-sub" x="21" y="25.5" textAnchor="middle">
            {centerSub}
          </text>
        )}
      </svg>
      {legend && (
        <ul className="donut-legend">
          {segments.map((seg) => (
            <li className="donut-legend-item" key={seg.label}>
              <span className="donut-legend-dot" style={{ background: seg.color }} />
              <span className="donut-legend-label">{seg.label}</span>
              <span className="donut-legend-value">{seg.value}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
