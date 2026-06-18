// Anomaly sparkline — plots the finding's REAL series (from the job's detail payload): the
// learned baseline band (mean ± σ, series-1 @14%), the dashed expected mean, the actual line, and
// the recent/anomalous segment redrawn in the severity colour with a marker over the recent
// window. A small inline SVG (the band + split-colour segment is awkward in uPlot, and these are
// tiny static sparklines, not interactive charts).

import type { AnomalyDetail } from '../types/api';

const W = 300;
const H = 60;
const PADX = 2;
const PADT = 6;
const PADB = 6;

export function AnomalyChart({
  detail,
  severity,
}: {
  detail: AnomalyDetail;
  severity: 'crit' | 'warn' | 'info';
}) {
  const pts = detail.points;
  if (!pts || pts.length < 2) return null;

  const plotW = W - PADX * 2;
  const plotH = H - PADT - PADB;
  const n = pts.length;

  const vals = pts.map((p) => p.v);
  const lo = Math.min(...vals, detail.mean - detail.sigma) - 0.4;
  const hi = Math.max(...vals, detail.mean + detail.sigma) + 0.4;
  const span = hi - lo || 1;
  const xAt = (i: number) => PADX + (i / (n - 1)) * plotW;
  const yAt = (v: number) => PADT + plotH - ((v - lo) / span) * plotH;

  // Baseline band (mean ± σ) as a horizontal rect.
  const bandTop = yAt(detail.mean + detail.sigma);
  const bandBot = yAt(detail.mean - detail.sigma);

  // Split the actual line at the first recent point so the recent segment is drawn in the
  // severity colour (and the marker rect spans the recent window).
  const firstRecent = pts.findIndex((p) => p.recent);
  const splitAt = firstRecent < 0 ? n - 1 : Math.max(0, firstRecent - 1);

  let normal = '';
  for (let i = 0; i <= splitAt; i++) {
    normal += `${i ? 'L' : 'M'}${xAt(i).toFixed(1)} ${yAt(pts[i].v).toFixed(1)} `;
  }
  let recent = '';
  for (let i = splitAt; i < n; i++) {
    recent += `${i === splitAt ? 'M' : 'L'}${xAt(i).toFixed(1)} ${yAt(pts[i].v).toFixed(1)} `;
  }
  const mx0 = xAt(splitAt);
  const markerW = Math.max(0, xAt(n - 1) - mx0);

  return (
    <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-hidden>
      <rect
        className="ts-ac-band"
        x={PADX}
        y={Math.min(bandTop, bandBot).toFixed(1)}
        width={plotW}
        height={Math.abs(bandBot - bandTop).toFixed(1)}
      />
      {markerW > 0 && (
        <rect
          className="ts-ac-marker"
          x={mx0.toFixed(1)}
          y={PADT}
          width={markerW.toFixed(1)}
          height={plotH}
          fill="var(--bg-tertiary)"
        />
      )}
      <line
        className="ts-ac-expected"
        x1={PADX}
        y1={yAt(detail.mean).toFixed(1)}
        x2={W - PADX}
        y2={yAt(detail.mean).toFixed(1)}
      />
      <path className="ts-ac-line" d={normal.trim()} />
      <path className={`ts-ac-anom ${severity}`} d={recent.trim()} />
    </svg>
  );
}
