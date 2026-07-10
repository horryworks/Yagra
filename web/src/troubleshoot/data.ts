// Troubleshoot — the static tool catalog (UI metadata only). Runs and findings are now real,
// served by the jobs API (ADR-022) — see services/api.ts and store.ts. This file holds just the
// fixed set of diagnostics and their display metadata (monogram, method colour, copy), plus the
// anomaly-kind palette the report uses.
//
// Method and anomaly-kind colours come from the categorical SERIES palette (tokens.css) — they
// classify, they are NOT status and NOT brand. Reference the token, never a raw hex.
//
// i18n: this is a non-component module, so the human-readable fields hold i18next KEYS (under the
// `troubleshoot` namespace), not English — the call sites resolve them with `t()` at render time
// (see rules; a module-load `t()` would freeze one language). Only structural/technical fields
// (id, mono, method, depth, color, reportPath) are literals.
//
// `Tool.id` matches the backend `AnalysisToolKey` (anomaly | correlation | capacity | flap).

import type { AnalysisToolKey } from '../types/api';

/** Analysis technique behind a tool — colours it on the card (categorical, series palette). */
export type Method = 'stat' | 'ml' | 'topo' | 'probe';

export interface MethodMeta {
  /** i18next key (troubleshoot ns) for the method label — resolve with `t()` at the call site. */
  label: string;
  /** A `var(--series-N)` token — categorical, carries no status meaning. */
  color: string;
}

export const METHODS: Record<Method, MethodMeta> = {
  stat: { label: 'methods.stat', color: 'var(--series-1)' },
  ml: { label: 'methods.ml', color: 'var(--series-2)' },
  topo: { label: 'methods.topo', color: 'var(--series-3)' },
  probe: { label: 'methods.probe', color: 'var(--series-4)' },
};

export interface Tool {
  /** Matches the backend tool key. */
  id: AnalysisToolKey;
  /** Monospace monogram tile (2 chars). */
  mono: string;
  /** i18next key (troubleshoot ns) — resolve with `t()`. */
  name: string;
  method: Method;
  /** Compute depth 1–5 (drives the pip indicator). */
  depth: number;
  /** i18next key for the human estimate (e.g. resolves to "~2–4 min"). */
  est: string;
  /** i18next key for the scope hint shown under the drawer's scope field. */
  scope: string;
  /** i18next key for the description. */
  desc: string;
  /** i18next key for the "Surfaces …" reveal line. */
  reveal: string;
  /** Report route, only for tools whose report screen exists (Anomaly today). */
  reportPath?: string;
}

export const TOOLS: Tool[] = [
  {
    id: 'anomaly',
    mono: 'An',
    name: 'tools.anomaly.name',
    method: 'ml',
    depth: 4,
    est: 'tools.anomaly.est',
    scope: 'tools.anomaly.scope',
    desc: 'tools.anomaly.desc',
    reveal: 'tools.anomaly.reveal',
    reportPath: '/troubleshoot/anomaly',
  },
  {
    id: 'correlation',
    mono: 'Co',
    name: 'tools.correlation.name',
    method: 'stat',
    depth: 3,
    est: 'tools.correlation.est',
    scope: 'tools.correlation.scope',
    desc: 'tools.correlation.desc',
    reveal: 'tools.correlation.reveal',
  },
  {
    id: 'capacity',
    mono: 'Cf',
    name: 'tools.capacity.name',
    method: 'stat',
    depth: 4,
    est: 'tools.capacity.est',
    scope: 'tools.capacity.scope',
    desc: 'tools.capacity.desc',
    reveal: 'tools.capacity.reveal',
  },
  {
    id: 'flap',
    mono: 'Fl',
    name: 'tools.flap.name',
    method: 'stat',
    depth: 2,
    est: 'tools.flap.est',
    scope: 'tools.flap.scope',
    desc: 'tools.flap.desc',
    reveal: 'tools.flap.reveal',
  },
];

export function toolById(id: string): Tool | undefined {
  return TOOLS.find((t) => t.id === id);
}

/** Anomaly shape — colours the kind chip and the redrawn anomalous segment (series palette). */
export type Kind = 'spike' | 'level' | 'drift' | 'flat' | 'season';

export interface KindMeta {
  /** i18next key (troubleshoot ns) for the kind label — resolve with `t()` at the call site. */
  label: string;
  color: string;
}

export const KINDS: Record<Kind, KindMeta> = {
  spike: { label: 'kinds.spike', color: 'var(--series-4)' },
  level: { label: 'kinds.level', color: 'var(--series-5)' },
  drift: { label: 'kinds.drift', color: 'var(--series-6)' },
  flat: { label: 'kinds.flat', color: 'var(--series-3)' },
  season: { label: 'kinds.season', color: 'var(--series-2)' },
};

/** Catalog metadata for a finding kind. Anomaly shapes carry an i18next label key (resolve with
 *  `t()`); an unknown kind falls back to its raw id (which `t()` echoes unchanged) + neutral colour. */
export function kindMeta(kind: string): { label: string; color: string } {
  return (KINDS as Record<string, KindMeta>)[kind] ?? { label: kind, color: 'var(--text-tertiary)' };
}
