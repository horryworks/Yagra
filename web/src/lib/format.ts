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
