// Troubleshoot — the static tool catalog (UI metadata only). Runs and findings are now real,
// served by the jobs API (ADR-022) — see services/api.ts and store.ts. This file holds just the
// fixed set of diagnostics and their display metadata (monogram, method colour, copy), plus the
// anomaly-kind palette the report uses.
//
// Method and anomaly-kind colours come from the categorical SERIES palette (tokens.css) — they
// classify, they are NOT status and NOT brand. Reference the token, never a raw hex.
//
// `Tool.id` matches the backend `AnalysisToolKey` (anomaly | correlation | capacity | flap).

import type { AnalysisToolKey } from '../types/api';

/** Analysis technique behind a tool — colours it on the card (categorical, series palette). */
export type Method = 'stat' | 'ml' | 'topo' | 'probe';

export interface MethodMeta {
  label: string;
  /** A `var(--series-N)` token — categorical, carries no status meaning. */
  color: string;
}

export const METHODS: Record<Method, MethodMeta> = {
  stat: { label: 'Statistical', color: 'var(--series-1)' },
  ml: { label: 'ML', color: 'var(--series-2)' },
  topo: { label: 'Topology', color: 'var(--series-3)' },
  probe: { label: 'Active probe', color: 'var(--series-4)' },
};

export interface Tool {
  /** Matches the backend tool key. */
  id: AnalysisToolKey;
  /** Monospace monogram tile (2 chars). */
  mono: string;
  name: string;
  method: Method;
  /** Compute depth 1–5 (drives the pip indicator). */
  depth: number;
  /** Human estimate, e.g. "~2–4 min". */
  est: string;
  /** Scope hint shown under the drawer's scope field. */
  scope: string;
  desc: string;
  /** "Surfaces …" reveal line. */
  reveal: string;
  /** Report route, only for tools whose report screen exists (Anomaly today). */
  reportPath?: string;
}

export const TOOLS: Tool[] = [
  {
    id: 'anomaly',
    mono: 'An',
    name: 'Anomaly Detection',
    method: 'ml',
    depth: 4,
    est: '~2–4 min',
    scope: 'node group · metric families',
    desc: 'Learns each metric’s own baseline and flags statistically significant deviations — slow drifts and odd shapes that fixed thresholds never trip.',
    reveal: 'spikes · level shifts · stuck counters · seasonality breaks',
    reportPath: '/troubleshoot/anomaly',
  },
  {
    id: 'correlation',
    mono: 'Co',
    name: 'Event Correlation',
    method: 'stat',
    depth: 3,
    est: '~1–3 min',
    scope: 'incident time window',
    desc: 'Cross-correlates metrics inside a window to surface what actually moved together — not just what alarmed.',
    reveal: 'co-moving series · lead/lag relationships',
  },
  {
    id: 'capacity',
    mono: 'Cf',
    name: 'Capacity Forecast',
    method: 'stat',
    depth: 4,
    est: '~2–5 min',
    scope: 'resources · utilization %',
    desc: 'Projects resource utilization forward by trend to estimate when each will hit exhaustion.',
    reveal: 'time-to-exhaustion · growth rate',
  },
  {
    id: 'flap',
    mono: 'Fl',
    name: 'Flap Analysis',
    method: 'stat',
    depth: 2,
    est: '~1–2 min',
    scope: 'reachability · 24h–7d',
    desc: 'Scans reachability state churn to surface nodes that bounce up/down and quietly drop sessions.',
    reveal: 'flap rate · churn windows',
  },
];

export function toolById(id: string): Tool | undefined {
  return TOOLS.find((t) => t.id === id);
}

/** Anomaly shape — colours the kind chip and the redrawn anomalous segment (series palette). */
export type Kind = 'spike' | 'level' | 'drift' | 'flat' | 'season';

export interface KindMeta {
  label: string;
  color: string;
}

export const KINDS: Record<Kind, KindMeta> = {
  spike: { label: 'Spike', color: 'var(--series-4)' },
  level: { label: 'Level shift', color: 'var(--series-5)' },
  drift: { label: 'Trend drift', color: 'var(--series-6)' },
  flat: { label: 'Stuck / flatline', color: 'var(--series-3)' },
  season: { label: 'Seasonality break', color: 'var(--series-2)' },
};

/** Catalog metadata for a finding kind (anomaly shapes have a palette entry; others are neutral). */
export function kindMeta(kind: string): { label: string; color: string } {
  return (KINDS as Record<string, KindMeta>)[kind] ?? { label: kind, color: 'var(--text-tertiary)' };
}
