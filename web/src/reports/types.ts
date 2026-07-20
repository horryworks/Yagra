// SPDX-License-Identifier: AGPL-3.0-only
// Frontend helpers for the report document (spec). The backend stores `spec` opaquely and the
// WebUI owns the shape (same contract as the dashboard layout) — these helpers build/sanitize it.

import type { TFunction } from 'i18next';
import type {
  ReportFrequency,
  ReportSchedule,
  ReportSectionDef,
  ReportSectionInstance,
  ReportSpec,
} from '../types/api';

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

/** Title for a section kind from the catalog (falls back to the raw kind). */
export function sectionTitle(defs: ReportSectionDef[], kind: string): string {
  return defs.find((d) => d.kind === kind)?.title ?? kind;
}

const WEEKDAY_KEYS = ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat'];

/** Weekday name for 0=Sun..6=Sat (clamped). `t` is threaded in (non-component module) so the label
 *  re-resolves on language change. */
export function weekdayName(t: TFunction, dow: number): string {
  const key = WEEKDAY_KEYS[Math.max(0, Math.min(6, dow))] ?? 'sun';
  return t(`reports:weekday.${key}`);
}

/** Human-readable cadence for a schedule (e.g. "Weekly · Monday 09:00 UTC"). Word order comes from
 *  the translation so a language like Japanese can reorder it. */
export function cadenceLabel(
  t: TFunction,
  s: {
    frequency: ReportFrequency;
    day_of_week: number | null;
    day_of_month: number | null;
    at_hour: number;
    at_minute: number;
  },
): string {
  const time = `${String(s.at_hour).padStart(2, '0')}:${String(s.at_minute).padStart(2, '0')} UTC`;
  switch (s.frequency) {
    case 'weekly':
      return t('reports:cadence.weekly', { day: weekdayName(t, s.day_of_week ?? 0), time });
    case 'monthly':
      return t('reports:cadence.monthly', { day: s.day_of_month ?? 1, time });
    default:
      return t('reports:cadence.daily', { time });
  }
}

export const WEEKDAY_OPTIONS = WEEKDAY_KEYS.map((key, value) => ({
  value,
  labelKey: `reports:weekday.${key}`,
}));

/** Whether a schedule is the cheapest "next run is soon" — for sorting display (unused stub kept
 *  small; the list is server-ordered by next_run_at). */
export function isScheduleDue(s: ReportSchedule, now = Date.now()): boolean {
  return s.enabled && s.next_run_ms <= now;
}
