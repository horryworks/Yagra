// Pure presentation helpers. Colors resolve to theme CSS variables (ui-conventions) — no
// hardcoded hex here, so the theme stays the single source of truth.

import type { MetricPoint, NodeState, Severity } from '../types/api';

/** Split time-series points into the parallel `[timestamps, values]` uPlot wants. */
export function pointsToSeries(points: MetricPoint[]): {
  timestamps: number[];
  values: number[];
} {
  return {
    timestamps: points.map((p) => p.t),
    values: points.map((p) => p.v),
  };
}

/** CSS variable holding the color for a severity. Severity maps onto the status palette
   (critical/warning); 'info' is not a network status, so it borrows a categorical color. */
export function severityColorVar(severity: Severity): string {
  switch (severity) {
    case 'critical':
      return 'var(--status-critical)';
    case 'warning':
      return 'var(--status-warning)';
    case 'info':
      return 'var(--severity-info)';
  }
}

/** CSS variable holding the color for a node state (design-system §1.3 status semantics). */
export function stateColorVar(state: NodeState): string {
  switch (state) {
    case 'ok':
      return 'var(--status-up)';
    case 'warning':
      return 'var(--status-warning)';
    case 'critical':
      return 'var(--status-critical)';
    case 'unreachable':
      return 'var(--status-unreachable)';
    case 'unknown':
      return 'var(--status-unknown)';
    case 'maintenance':
      return 'var(--status-maintenance)';
  }
}

/** Human label for a state. */
export function stateLabel(state: NodeState): string {
  return state.charAt(0).toUpperCase() + state.slice(1);
}

/** Rank a severity for sorting (higher = worse). */
export function severityRank(severity: Severity): number {
  return { info: 0, warning: 1, critical: 2 }[severity];
}

/** Format a Unix-ms timestamp as a local time string. */
export function formatTimestamp(unixMs: number): string {
  return new Date(unixMs).toLocaleString();
}

/** Format a millisecond RTT value. */
export function formatRtt(ms: number): string {
  return `${ms.toFixed(1)} ms`;
}

/** Format a bits-per-second rate with SI-ish units (k/M/G), or `—` when unknown. */
export function formatBps(bps: number | null): string {
  if (bps == null) return '—';
  const units = ['bps', 'kbps', 'Mbps', 'Gbps', 'Tbps'];
  let v = bps;
  let u = 0;
  while (v >= 1000 && u < units.length - 1) {
    v /= 1000;
    u += 1;
  }
  return `${v.toFixed(v >= 100 || u === 0 ? 0 : 1)} ${units[u]}`;
}

/** Format a utilization percentage, or `—` when unknown (no speed / no data). */
export function formatUtil(pct: number | null): string {
  return pct == null ? '—' : `${pct.toFixed(pct >= 10 ? 0 : 1)}%`;
}

/** Format SNMP TimeTicks (hundredths of a second) as a compact human uptime, e.g.
 *  `1y 2mo 3d 12:34`. The date head uses distinct, spaced unit suffixes — `y` / `mo` / `d` — so
 *  month never collides with the minutes that live after the `HH:MM` colon. Larger zero units are
 *  dropped (39 days reads `1mo 9d 02:09`, not `0y 1mo 9d …`); `HH:MM` is always shown, zero-padded.
 *  Months/years are approximate (30d / 365d) — fine for an at-a-glance uptime. Returns `—` for a
 *  missing/negative value. */
export function formatUptimeTicks(ticks: number): string {
  if (!Number.isFinite(ticks) || ticks < 0) return '—';
  let secs = Math.floor(ticks / 100);
  const YEAR = 365 * 86400;
  const MONTH = 30 * 86400;
  const years = Math.floor(secs / YEAR);
  secs -= years * YEAR;
  const months = Math.floor(secs / MONTH);
  secs -= months * MONTH;
  const days = Math.floor(secs / 86400);
  secs -= days * 86400;
  const hours = Math.floor(secs / 3600);
  secs -= hours * 3600;
  const minutes = Math.floor(secs / 60);
  const parts: string[] = [];
  if (years > 0) parts.push(`${years}y`);
  if (parts.length || months > 0) parts.push(`${months}mo`);
  if (parts.length || days > 0) parts.push(`${days}d`);
  const hm = `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}`;
  return parts.length ? `${parts.join(' ')} ${hm}` : hm;
}

/** Friendly display labels for known scalar SNMP metrics; falls back to the raw metric name. */
const SCALAR_LABELS: Record<string, string> = {
  snmp_sys_uptime_ticks: 'Uptime',
};

/** A known scalar gets a human label + formatted value (and renders in the UI font, not mono);
 *  an unknown one keeps its raw OID-ish metric name + numeric value (mono). */
export function scalarDisplay(metric: string, value: number): {
  label: string;
  value: string;
  known: boolean;
} {
  if (metric === 'snmp_sys_uptime_ticks') {
    return { label: SCALAR_LABELS[metric], value: formatUptimeTicks(value), known: true };
  }
  const known = metric in SCALAR_LABELS;
  return { label: SCALAR_LABELS[metric] ?? metric, value: String(value), known };
}

/** Compact, unit-less SI suffix (k/M/G/T) for a plain number — for chart axis ticks so big
 *  values (e.g. 455000) render as "455k" instead of being clipped. */
export function formatSi(n: number): string {
  const abs = Math.abs(n);
  const units: [number, string][] = [
    [1e12, 'T'],
    [1e9, 'G'],
    [1e6, 'M'],
    [1e3, 'k'],
  ];
  for (const [div, suffix] of units) {
    if (abs >= div) {
      const v = n / div;
      return `${v.toFixed(v >= 100 || Number.isInteger(v) ? 0 : 1)}${suffix}`;
    }
  }
  return Number.isInteger(n) ? String(n) : n.toFixed(1);
}
