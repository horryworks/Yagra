// SPDX-License-Identifier: AGPL-3.0-only
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
// `Tool.id` matches the backend `AnalysisToolKey` — the four metric analyses plus the passive-event
// (ADR-024) and flow (ADR-031) kinds; `data.test.ts` pins the set against that union.

import type { AnalysisToolKey } from '../types/api';

/** Analysis technique behind a tool — colours it on the card (categorical, series palette). */
export type Method = 'stat' | 'ml' | 'topo' | 'probe' | 'passive' | 'flow';

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
  passive: { label: 'methods.passive', color: 'var(--series-5)' },
  flow: { label: 'methods.flow', color: 'var(--series-6)' },
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
  /**
   * Report route — set **only** on tools that have a body in the report registry, so a "View →"
   * button never lands on a redirect. Always [`reportPathFor`]`(id)`; every tool gets one as the
   * per-tool reports land (see `report/registry.tsx`).
   */
  reportPath?: string;
}

/** The canonical report URL for a tool (`/troubleshoot/report/:tool`, see `report/registry.tsx`). */
export function reportPathFor(id: AnalysisToolKey): string {
  return `/troubleshoot/report/${id}`;
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
    reportPath: '/troubleshoot/report/anomaly',
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
    reportPath: '/troubleshoot/report/correlation',
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
    reportPath: '/troubleshoot/report/capacity',
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
    reportPath: '/troubleshoot/report/flap',
  },
  // ── Passive monitoring (events, ADR-024) ──
  {
    id: 'event_storm',
    mono: 'Es',
    name: 'tools.event_storm.name',
    method: 'passive',
    depth: 2,
    est: 'tools.event_storm.est',
    scope: 'tools.event_storm.scope',
    desc: 'tools.event_storm.desc',
    reveal: 'tools.event_storm.reveal',
    reportPath: '/troubleshoot/report/event_storm',
  },
  {
    id: 'event_flap',
    mono: 'Ef',
    name: 'tools.event_flap.name',
    method: 'passive',
    depth: 2,
    est: 'tools.event_flap.est',
    scope: 'tools.event_flap.scope',
    desc: 'tools.event_flap.desc',
    reveal: 'tools.event_flap.reveal',
    reportPath: '/troubleshoot/report/event_flap',
  },
  {
    id: 'severity_shift',
    mono: 'Sv',
    name: 'tools.severity_shift.name',
    method: 'passive',
    depth: 2,
    est: 'tools.severity_shift.est',
    scope: 'tools.severity_shift.scope',
    desc: 'tools.severity_shift.desc',
    reveal: 'tools.severity_shift.reveal',
    reportPath: '/troubleshoot/report/severity_shift',
  },
  {
    id: 'rule_gap',
    mono: 'Rg',
    name: 'tools.rule_gap.name',
    method: 'passive',
    depth: 1,
    est: 'tools.rule_gap.est',
    scope: 'tools.rule_gap.scope',
    desc: 'tools.rule_gap.desc',
    reveal: 'tools.rule_gap.reveal',
    reportPath: '/troubleshoot/report/rule_gap',
  },
  {
    id: 'auth_probe',
    mono: 'Au',
    name: 'tools.auth_probe.name',
    method: 'passive',
    depth: 1,
    est: 'tools.auth_probe.est',
    scope: 'tools.auth_probe.scope',
    desc: 'tools.auth_probe.desc',
    reveal: 'tools.auth_probe.reveal',
    reportPath: '/troubleshoot/report/auth_probe',
  },
  // ── Flow monitoring (ClickHouse, ADR-031) ──
  {
    id: 'traffic_anomaly',
    mono: 'Ta',
    name: 'tools.traffic_anomaly.name',
    method: 'flow',
    depth: 3,
    est: 'tools.traffic_anomaly.est',
    scope: 'tools.traffic_anomaly.scope',
    desc: 'tools.traffic_anomaly.desc',
    reveal: 'tools.traffic_anomaly.reveal',
    reportPath: '/troubleshoot/report/traffic_anomaly',
  },
  {
    id: 'talker_shift',
    mono: 'Ts',
    name: 'tools.talker_shift.name',
    method: 'flow',
    depth: 2,
    est: 'tools.talker_shift.est',
    scope: 'tools.talker_shift.scope',
    desc: 'tools.talker_shift.desc',
    reveal: 'tools.talker_shift.reveal',
    reportPath: '/troubleshoot/report/talker_shift',
  },
  {
    id: 'new_destination',
    mono: 'Nd',
    name: 'tools.new_destination.name',
    method: 'flow',
    depth: 2,
    est: 'tools.new_destination.est',
    scope: 'tools.new_destination.scope',
    desc: 'tools.new_destination.desc',
    reveal: 'tools.new_destination.reveal',
    reportPath: '/troubleshoot/report/new_destination',
  },
  {
    id: 'flow_scan',
    mono: 'Sc',
    name: 'tools.flow_scan.name',
    method: 'flow',
    depth: 2,
    est: 'tools.flow_scan.est',
    scope: 'tools.flow_scan.scope',
    desc: 'tools.flow_scan.desc',
    reveal: 'tools.flow_scan.reveal',
  },
  // ── Cross-store ──
  {
    id: 'saturation',
    mono: 'St',
    name: 'tools.saturation.name',
    method: 'flow',
    depth: 3,
    est: 'tools.saturation.est',
    scope: 'tools.saturation.scope',
    desc: 'tools.saturation.desc',
    reveal: 'tools.saturation.reveal',
  },
  {
    id: 'incident_correlate',
    mono: 'Ic',
    name: 'tools.incident_correlate.name',
    method: 'topo',
    depth: 4,
    est: 'tools.incident_correlate.est',
    scope: 'tools.incident_correlate.scope',
    desc: 'tools.incident_correlate.desc',
    reveal: 'tools.incident_correlate.reveal',
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
