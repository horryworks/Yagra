// Grid heatmap: a row-label column + a matrix of cells shaded by intensity, with a scale legend.
// Powers the alert calendar (weekday × hour) and the interface utilization heatmap. Cell color is
// a series base mixed toward the track color by intensity (color-mix) — the base is passed in so
// the widget owns the channel (series for counts, status for utilization).

import { useTranslation } from 'react-i18next';
import './Heatmap.css';

interface Props {
  rowLabels: string[];
  /** Column headers; pass '' for unlabeled columns (e.g. only every 6th hour). */
  colLabels: string[];
  /** values[row][col]; ragged rows are tolerated — a missing cell renders as a hatched
   *  "no data" cell, visually distinct from a real 0 (which shades as the track color). */
  values: number[][];
  /** Denominator for intensity; defaults to the max cell value. */
  max?: number;
  /** Base CSS color the cells mix from at full intensity (default series-1). */
  colorBase?: string;
  /** Cell hover/title formatter. */
  title?: (row: string, col: string, value: number) => string;
}

export function Heatmap({ rowLabels, colLabels, values, max, colorBase = 'var(--series-1)', title }: Props) {
  const { t: tr } = useTranslation('dashboard');
  const peak = max ?? Math.max(1, ...values.flat());
  return (
    <div className="heatmap" style={{ ['--heat-cols' as string]: colLabels.length }}>
      <div className="heatmap-corner" />
      {colLabels.map((c, i) => (
        <div className="heatmap-colhead" key={`c${i}`}>
          {c}
        </div>
      ))}
      {rowLabels.map((row, r) => (
        <div className="heatmap-rowgroup" key={`r${r}`} style={{ display: 'contents' }}>
          <div className="heatmap-rowhead">{row}</div>
          {colLabels.map((_, c) => {
            const raw = values[r]?.[c];
            const missing = raw == null;
            const v = raw ?? 0;
            const t = peak > 0 ? Math.min(100, (v / peak) * 100) : 0;
            // 0 stays the track color; higher values mix toward the base. A missing cell is
            // hatched (.heatmap-cell-empty) so "no data" can't be misread as 0%.
            const bg = `color-mix(in oklab, ${colorBase} ${t}%, var(--bg-tertiary))`;
            return (
              <div
                className={missing ? 'heatmap-cell heatmap-cell-empty' : 'heatmap-cell'}
                key={`${r}-${c}`}
                style={missing ? undefined : { background: bg }}
                title={
                  missing
                    ? `${row} · ${colLabels[c]}: ${tr('primitives.heatmap.noData')}`
                    : title
                      ? title(row, colLabels[c], v)
                      : String(v)
                }
              />
            );
          })}
        </div>
      ))}
    </div>
  );
}
