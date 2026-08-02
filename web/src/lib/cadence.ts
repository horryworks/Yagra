// SPDX-License-Identifier: AGPL-3.0-only
// How a preset firing cadence is presented — as a map keyed by the union, not a switch.
//
// Here rather than in `reports/` because two features schedule things now: a report definition and
// a Troubleshoot analysis. They share one backend type (`Cadence`, Rust `crate::cadence`), so they
// share one presentation of it — a second copy is how "weekly on Monday" and "weekly on Mon" end up
// on two screens, and how an unknown cadence ends up rendered as "Daily" on one of them.
//
// The label keys stay in the `reports` namespace, fully qualified. They are bundled eagerly for
// every namespace (`i18n.ts`), so `reports:cadence.weekly` resolves from a Troubleshoot screen just
// as well; moving the strings would churn both locale files to no operator-visible effect.

import type { TFunction } from 'i18next';
import { CADENCES, type Cadence } from '../types/api';

/** Cadence label key + which extra field it interpolates.
 *
 *  A `Record` and not a `switch`: `cadenceLabel` was a switch ending in `default:`, which rendered
 *  a cadence this build does not know as **"Daily"** — the one reading that is actively misleading,
 *  because the operator then believes a monthly schedule fires every night. */
export const CADENCE: Record<Cadence, { labelKey: string; part: 'day' | 'dom' | 'none' }> = {
  daily: { labelKey: 'reports:cadence.daily', part: 'none' },
  weekly: { labelKey: 'reports:cadence.weekly', part: 'day' },
  monthly: { labelKey: 'reports:cadence.monthly', part: 'dom' },
  unknown: { labelKey: 'reports:cadence.unknown', part: 'none' },
};

/** The cadences an operator may actually choose — `unknown` is a storage degradation, never a
 *  selectable option. A deliberate subset of the union, pinned by a test (the `monitorKinds.ts`
 *  pattern) so a fourth cadence forces the question rather than being silently excluded. */
export const SELECTABLE_CADENCES = CADENCES.filter(
  (f) => f !== 'unknown',
) as readonly Exclude<Cadence, 'unknown'>[];

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
  // The day fields are optional as well as nullable: a stored schedule simply omits the one its
  // cadence does not use, and Rust's Option<i16> generates as `number | null | undefined`.
  // Accepting that here removes the narrowing wrapper each calling page would otherwise need —
  // and there were about to be two of them.
  s: {
    frequency: Cadence;
    day_of_week?: number | null;
    day_of_month?: number | null;
    at_hour: number;
    at_minute: number;
  },
): string {
  const time = `${String(s.at_hour).padStart(2, '0')}:${String(s.at_minute).padStart(2, '0')} UTC`;
  // Registry-driven, not a switch: the `default:` this replaces rendered an unrecognised cadence as
  // "Daily", which is a claim about when the report fires rather than an admission of ignorance.
  const spec = CADENCE[s.frequency];
  switch (spec.part) {
    case 'day':
      return t(spec.labelKey, { day: weekdayName(t, s.day_of_week ?? 0), time });
    case 'dom':
      return t(spec.labelKey, { day: s.day_of_month ?? 1, time });
    case 'none':
      return t(spec.labelKey, { time });
  }
}

export const WEEKDAY_OPTIONS = WEEKDAY_KEYS.map((key, value) => ({
  value,
  labelKey: `reports:weekday.${key}`,
}));
