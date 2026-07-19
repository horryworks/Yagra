// Ranked horizontal bars: label · track+fill · right-aligned value. Powers all Top-N widgets
// (RTT, CPU, memory, talkers, top alerting nodes). Bar width is proportional to the value
// relative to the largest row, so the ranking reads at a glance. The fill color is a series
// color by default (Top-N is categorical, not a node status); callers may override per row.

import { useTranslation } from 'react-i18next';
import './RankedBars.css';

export interface RankedRow {
  /** Stable key + left label (e.g. node name). */
  label: string;
  /** Magnitude that drives the bar width. */
  value: number;
  /** Right-aligned display string (e.g. "44 ms", "87%"); defaults to the value. */
  valueText?: string;
  /** Fill color (CSS var); defaults to the shared series-1. */
  color?: string;
}

interface Props {
  rows: RankedRow[];
  /** Denominator for bar widths; defaults to the max row value (so the top row fills the track). */
  max?: number;
  /** Message when there are no rows. */
  empty?: string;
  /** When set, rows become clickable drill-downs (keyboard-operable); receives the row's index. */
  onRowClick?: (index: number) => void;
}

export function RankedBars({ rows, max, empty, onRowClick }: Props) {
  const { t } = useTranslation('dashboard');
  if (rows.length === 0) return <p className="muted">{empty ?? t('primitives.rankedBars.empty')}</p>;
  const peak = max ?? Math.max(...rows.map((r) => r.value), 0);
  const clickable = Boolean(onRowClick);
  return (
    <ul className="rankedbars">
      {rows.map((r, i) => {
        const pct = peak > 0 ? Math.max(0, Math.min(100, (r.value / peak) * 100)) : 0;
        return (
          <li
            className={clickable ? 'rankedbar clickable' : 'rankedbar'}
            key={r.label}
            role={clickable ? 'button' : undefined}
            tabIndex={clickable ? 0 : undefined}
            onClick={onRowClick ? () => onRowClick(i) : undefined}
            onKeyDown={
              onRowClick
                ? (e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault();
                      onRowClick(i);
                    }
                  }
                : undefined
            }
          >
            <span className="rankedbar-label" title={r.label}>
              {r.label}
            </span>
            <span className="rankedbar-track">
              <span
                className="rankedbar-fill"
                style={{ width: `${pct}%`, background: r.color ?? 'var(--series-1)' }}
              />
            </span>
            <span className="rankedbar-value">{r.valueText ?? String(r.value)}</span>
          </li>
        );
      })}
    </ul>
  );
}
