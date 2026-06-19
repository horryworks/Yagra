// Frontend helpers for the report document (spec). The backend stores `spec` opaquely and the
// WebUI owns the shape (same contract as the dashboard layout) — these helpers build/sanitize it.

import type {
  ReportFrequency,
  ReportSchedule,
  ReportSectionDef,
  ReportSectionInstance,
  ReportSpec,
} from '../types/api';

/** Time-range presets for a report window. */
export const RANGE_OPTIONS: { label: string; secs: number }[] = [
  { label: 'Last 24 hours', secs: 24 * 3600 },
  { label: 'Last 7 days', secs: 7 * 86400 },
  { label: 'Last 30 days', secs: 30 * 86400 },
  { label: 'Last 90 days', secs: 90 * 86400 },
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

const WEEKDAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];

/** Weekday name for 0=Sun..6=Sat (clamped). */
export function weekdayName(dow: number): string {
  return WEEKDAYS[Math.max(0, Math.min(6, dow))] ?? 'Sunday';
}

/** Human-readable cadence for a schedule (e.g. "Weekly · Monday 09:00 UTC"). */
export function cadenceLabel(s: {
  frequency: ReportFrequency;
  day_of_week: number | null;
  day_of_month: number | null;
  at_hour: number;
  at_minute: number;
}): string {
  const time = `${String(s.at_hour).padStart(2, '0')}:${String(s.at_minute).padStart(2, '0')} UTC`;
  switch (s.frequency) {
    case 'weekly':
      return `Weekly · ${weekdayName(s.day_of_week ?? 0)} ${time}`;
    case 'monthly':
      return `Monthly · day ${s.day_of_month ?? 1} ${time}`;
    default:
      return `Daily · ${time}`;
  }
}

export const WEEKDAY_OPTIONS = WEEKDAYS.map((label, value) => ({ value, label }));

/** Whether a schedule is the cheapest "next run is soon" — for sorting display (unused stub kept
 *  small; the list is server-ordered by next_run_at). */
export function isScheduleDue(s: ReportSchedule, now = Date.now()): boolean {
  return s.enabled && s.next_run_ms <= now;
}
