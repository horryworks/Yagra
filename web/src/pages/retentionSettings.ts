// SPDX-License-Identifier: AGPL-3.0-only
// Pure logic behind the Settings ▸ System settings retention card (ADR-040).
//
// It lives in a `.ts` and not in the page because Vitest runs with `include: ['src/**/*.test.ts']`
// — a test written in a `.tsx` is a file nothing runs (testing.md). The page keeps layout; the
// judgement (which rows are editable, what a store-owned row should read, whether a form is valid)
// is here where it can be tested.

import type { RetentionRow, RetentionValues } from '../types/api';

// Mirror the backend bands (yagra-core retention.rs MIN/MAX_RETENTION_*); the server re-validates
// and is authoritative.
export const MIN_DAYS = 1;
export const MAX_DAYS = 3650;
export const MIN_HOURS = 1;
export const MAX_HOURS = 8760;

/** The four editable windows, keyed the way the API names them. */
export const RETENTION_FIELDS = [
  'alert_linked_days',
  'unmatched_event_hours',
  'report_run_days',
  'flow_days',
] as const;
export type RetentionField = (typeof RETENTION_FIELDS)[number];

/** Every retained subject the backend can list (`retention::Subject::ALL`).
 *
 *  Hand-maintained rather than derived, because the card renders each row's name from
 *  `t(`settings.retention.subject.${row.subject}`)` — a key built at runtime from a server value,
 *  so nothing else can prove the strings exist. `i18nEnumKeys.test.ts` iterates this. A subject the
 *  server sends that is missing here is not an error (the card falls back to the raw token); it is
 *  a reminder that this list has to be extended in the same change. */
export const RETENTION_SUBJECTS = [
  'alert_history',
  'node_state_snapshots',
  'dns_chain_changes',
  'neighbor_changes',
  'l3_changes',
  'events_matched',
  'events_unmatched',
  'report_runs',
  'flow_records',
  'event_log_store',
  'metrics',
  'audit_log',
] as const;
export type RetentionSubject = (typeof RETENTION_SUBJECTS)[number];

/** The form's raw text state — one string per editable window. */
export type RetentionForm = Record<RetentionField, string>;

/** How a row should be rendered. Exhaustive over the backend's `tunable` tokens. */
export type RowMode = 'editable' | 'store-owned' | 'unlimited' | 'unknown';

/**
 * Which of the three presentations a row gets.
 *
 * `unknown` exists rather than a default-to-editable: a row whose `tunable` token this build does
 * not recognise came from a newer core, and rendering it as a control would offer an operator an
 * input that writes to a field this build cannot send.
 */
export function rowMode(row: Pick<RetentionRow, 'tunable'>): RowMode {
  switch (row.tunable) {
    case 'settings':
      return 'editable';
    case 'store_flag_read_only':
      return 'store-owned';
    case 'by_decision':
      return 'unlimited';
    default:
      return 'unknown';
  }
}

/** The `field` of a row, when it names one this build can edit. */
export function rowField(row: Pick<RetentionRow, 'field'>): RetentionField | null {
  return (RETENTION_FIELDS as readonly string[]).includes(row.field)
    ? (row.field as RetentionField)
    : null;
}

/** What a store-owned row's value cell should say. */
export type StoreValue =
  | { kind: 'reported'; value: string }
  | { kind: 'not-configured' }
  | { kind: 'unknown' };

/**
 * Read a store-owned row's value.
 *
 * Three outcomes on purpose. `store_reported` is absent both when the store is unreachable and when
 * it is running its own built-in default (`/flags` lists only flags that were explicitly set), and
 * in neither case do we know the number — so the cell says so instead of showing a plausible guess
 * that would be wrong exactly when an operator most needs it right.
 */
export function storeValue(
  row: Pick<RetentionRow, 'store_reported' | 'store_configured'>,
): StoreValue {
  if (!row.store_configured) return { kind: 'not-configured' };
  const reported = row.store_reported;
  if (typeof reported === 'string' && reported.trim() !== '') {
    return { kind: 'reported', value: reported.trim() };
  }
  return { kind: 'unknown' };
}

/** Whether a field is denominated in hours rather than days. */
export function isHours(field: RetentionField): boolean {
  return field === 'unmatched_event_hours';
}

/** The allowed band for a field, as `[min, max]`. */
export function bandFor(field: RetentionField): [number, number] {
  return isHours(field) ? [MIN_HOURS, MAX_HOURS] : [MIN_DAYS, MAX_DAYS];
}

export type ParseResult =
  | { ok: true; values: RetentionValues }
  | { ok: false; field: RetentionField; min: number; max: number };

/**
 * Validate the form and produce the request body.
 *
 * Reports the *first offending field* rather than a generic "check your input", because the card
 * has four inputs with two different units and "enter a number between 1 and 3650" next to an
 * hours field would be actively misleading.
 */
export function parseRetentionForm(form: RetentionForm): ParseResult {
  const values: Partial<Record<RetentionField, number>> = {};
  for (const field of RETENTION_FIELDS) {
    const [min, max] = bandFor(field);
    const n = Number(form[field].trim());
    if (form[field].trim() === '' || !Number.isInteger(n) || n < min || n > max) {
      return { ok: false, field, min, max };
    }
    values[field] = n;
  }
  return { ok: true, values: values as RetentionValues };
}

/** Seed the form's text state from the values the API returned. */
export function formFromValues(values: RetentionValues): RetentionForm {
  return {
    alert_linked_days: String(values.alert_linked_days),
    unmatched_event_hours: String(values.unmatched_event_hours),
    report_run_days: String(values.report_run_days),
    flow_days: String(values.flow_days),
  };
}

/**
 * Whether saving would change anything, so the card can leave Save disabled on an untouched form.
 * Compared numerically: `"30"` and `"30 "` are the same window.
 */
export function isDirty(form: RetentionForm, saved: RetentionValues): boolean {
  return RETENTION_FIELDS.some((f) => Number(form[f].trim()) !== saved[f]);
}
