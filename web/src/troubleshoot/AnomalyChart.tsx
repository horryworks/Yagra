// Anomaly sparkline: a hand-built SVG band chart (port of the handoff's `anomChart`). It draws
// the learned expected band (series-1 @14%), the dashed expected mean, the actual line, and the
// anomalous segment redrawn in the severity color with a marker rectangle over the anomaly
// window. Deterministic from the finding's `seed` so the shape is stable across renders. The
// shape varies by `kind` (spike / level / drift / flat / season). Pure presentation — given the
// custom band+segment geometry, a small inline SVG is the right tool here rather than uPlot.

import type { Anomaly } from './data';

const W = 300;
const H = 60;
const PADX = 2;
const PADT = 6;
const PADB = 6;
const N = 48;

export function AnomalyChart({ anomaly }: { anomaly: Anomaly }) {
  const plotW = W - PADX * 2;
  const plotH = H - PADT - PADB;

  // Deterministic LCG seeded from the finding — same shape every render.
  let x = (anomaly.seed * 2654435761) % 2147483647;
  const rnd = () => {
    x = (x * 1103515245 + 12345) & 0x7fffffff;
    return x / 0x7fffffff;
  };

  // Expected baseline: a gentle daily rhythm.
  const band = 1.0;
  const exp: number[] = [];
  for (let i = 0; i < N; i++) {
    exp.push(5 + Math.sin(i / 7 + anomaly.seed) * 1.4 + Math.sin(i / 3.3) * 0.5);
  }

  // Anomaly window: starts ~62% in; drift/flat run to the end, others span a short window.
  const aw = Math.floor(N * 0.62);
  const ae =
    anomaly.kind === 'drift' || anomaly.kind === 'flat' ? N - 1 : Math.min(N - 1, aw + 7);

  const act: number[] = [];
  for (let i = 0; i < N; i++) {
    let v = exp[i] + (rnd() - 0.5) * 0.5;
    if (i >= aw) {
      const t = (i - aw) / Math.max(1, N - 1 - aw);
      if (anomaly.kind === 'spike') {
        if (i <= ae) v = exp[i] + (3.5 - Math.abs(aw + 3 - i)) * 1.5;
      } else if (anomaly.kind === 'level') {
        v = exp[i] + 3.2;
      } else if (anomaly.kind === 'drift') {
        v = exp[i] + t * 4.2;
      } else if (anomaly.kind === 'flat') {
        v = exp[aw]; // stuck counter
      } else if (anomaly.kind === 'season') {
        v = exp[aw] + (rnd() - 0.5) * 0.4; // rhythm lost
      }
    }
    act.push(v);
  }

  const all = exp.concat(
    act,
    exp.map((e) => e + band),
    exp.map((e) => e - band),
  );
  const lo = Math.min(...all) - 0.4;
  const hi = Math.max(...all) + 0.4;
  const xAt = (i: number) => PADX + (i / (N - 1)) * plotW;
  const yAt = (v: number) => PADT + plotH - ((v - lo) / (hi - lo)) * plotH;

  // Expected band polygon (up across, down back).
  let up = '';
  let dn = '';
  exp.forEach((e, i) => {
    up += `${i ? 'L' : 'M'}${xAt(i).toFixed(1)} ${yAt(e + band).toFixed(1)} `;
  });
  for (let i = N - 1; i >= 0; i--) dn += `L${xAt(i).toFixed(1)} ${yAt(exp[i] - band).toFixed(1)} `;
  const bandPath = `${up}${dn}Z`;

  let expd = '';
  exp.forEach((e, i) => {
    expd += `${i ? 'L' : 'M'}${xAt(i).toFixed(1)} ${yAt(e).toFixed(1)} `;
  });

  // Normal segment (0..aw), anomaly segment (aw..end), plus tail back to normal after a spike.
  let norm = '';
  for (let i = 0; i <= aw; i++) norm += `${i ? 'L' : 'M'}${xAt(i).toFixed(1)} ${yAt(act[i]).toFixed(1)} `;
  const anomEnd = anomaly.kind === 'spike' ? ae : N - 1;
  let anom = '';
  for (let i = aw; i <= anomEnd; i++)
    anom += `${i === aw ? 'M' : 'L'}${xAt(i).toFixed(1)} ${yAt(act[i]).toFixed(1)} `;
  let tail = '';
  if (anomaly.kind === 'spike')
    for (let i = ae; i < N; i++)
      tail += `${i === ae ? 'M' : 'L'}${xAt(i).toFixed(1)} ${yAt(act[i]).toFixed(1)} `;

  const mx0 = xAt(aw);
  const mx1 = xAt(anomEnd);

  return (
    <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-hidden>
      <rect
        className="ts-ac-marker"
        x={mx0.toFixed(1)}
        y={PADT}
        width={(mx1 - mx0).toFixed(1)}
        height={plotH}
        fill="var(--bg-tertiary)"
      />
      <path className="ts-ac-band" d={bandPath} />
      <path className="ts-ac-expected" d={expd.trim()} />
      <path className="ts-ac-line" d={norm.trim()} />
      {tail && <path className="ts-ac-line" d={tail.trim()} />}
      <path className={`ts-ac-anom ${anomaly.sev}`} d={anom.trim()} />
    </svg>
  );
}
