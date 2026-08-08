// SPDX-License-Identifier: AGPL-3.0-only
// Frontend helpers for the report document (spec). The backend stores `spec` opaquely and the
// WebUI owns the shape (same contract as the dashboard layout) — these helpers build/sanitize it.

import type { ReportSectionDef, ReportSectionInstance, ReportSpec } from '../types/api';

/** Time-range presets for a report window. `labelKey` resolves in the reports namespace (this is a
 *  non-component module, so it stores i18n keys instead of resolving them at module load). */
export const RANGE_OPTIONS: { labelKey: string; secs: number }[] = [
  { labelKey: 'reports:builder.range.h24', secs: 24 * 3600 },
  { labelKey: 'reports:builder.range.d7', secs: 7 * 86400 },
  { labelKey: 'reports:builder.range.d30', secs: 30 * 86400 },
  { labelKey: 'reports:builder.range.d90', secs: 90 * 86400 },
];

/** Default report window (7 days) — mirrors the backend default. */
export const DEFAULT_RANGE_SECS = 7 * 86400;

/** A best-effort unique id for a placed section (crypto.randomUUID with a fallback). */
export function sectionId(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) {
    return crypto.randomUUID();
  }
  return `sec-${Math.random().toString(36).slice(2)}-${Date.now().toString(36)}`;
}

/** Default settings for a section from its catalog definition (each setting's `default`). */
export function defaultSettings(def: ReportSectionDef): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const s of def.settings) out[s.key] = s.default;
  return out;
}

/** A fresh placed section instance for a catalog kind. */
export function newSection(def: ReportSectionDef): ReportSectionInstance {
  return { id: sectionId(), kind: def.kind, settings: defaultSettings(def) };
}

/** An empty report spec (used when creating a new definition). */
export function emptySpec(): ReportSpec {
  return { version: 1, params: { range_secs: DEFAULT_RANGE_SECS }, sections: [] };
}

/** Coerce a stored/foreign spec into a usable shape (tolerates older/partial documents). */
export function sanitizeSpec(spec: ReportSpec | undefined | null): ReportSpec {
  if (!spec || typeof spec !== 'object') return emptySpec();
  const range =
    typeof spec.params?.range_secs === 'number' && spec.params.range_secs > 0
      ? spec.params.range_secs
      : DEFAULT_RANGE_SECS;
  const sections = Array.isArray(spec.sections)
    ? spec.sections
        .filter((s) => s && typeof s.kind === 'string')
        .map((s) => ({
          id: typeof s.id === 'string' ? s.id : sectionId(),
          kind: s.kind,
          settings: s.settings && typeof s.settings === 'object' ? s.settings : {},
        }))
    : [];
  return { version: 1, params: { range_secs: range }, sections };
}

