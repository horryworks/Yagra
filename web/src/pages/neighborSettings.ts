// SPDX-License-Identifier: AGPL-3.0-only
// Validation for the Settings ▸ System settings ▸ Neighbor discovery card (ADR-038).
//
// Lives beside `retentionSettings.ts` and for the same reason: Vitest only runs
// `src/**/*.test.ts`, so judgement written inside a .tsx is untested by construction (testing.md).

/** The cadence band the server enforces. Mirrored here so the form can refuse before a round trip;
 *  the server re-validates and is authoritative, and `GET /api/v1/settings/neighbors` reports the
 *  band it actually enforces so a UI can prefer that over this fallback. */
export const MIN_NEIGHBOR_INTERVAL_SECS = 300;
export const MAX_NEIGHBOR_INTERVAL_SECS = 86400;

/** The card's editable state, as typed. Kept as a string because that is what an input holds — a
 *  number would force a parse on every keystroke and make an in-progress "3" mean 3 seconds. */
export interface NeighborForm {
  enabled: boolean;
  intervalSecs: string;
}

export type NeighborParse =
  | { ok: true; values: { enabled: boolean; interval_secs: number } }
  | { ok: false; min: number; max: number };

/** Validate the form against the band the server reported (falling back to the compiled mirror).
 *
 *  The cadence is checked even when collection is switched **off**: the value is still stored, and
 *  saving an out-of-range number with the toggle off would fail server-side with an error that
 *  looks unrelated to the control the operator was actually using.
 */
export function parseNeighborForm(
  form: NeighborForm,
  band?: { min?: number | null; max?: number | null },
): NeighborParse {
  const min = band?.min != null && band.min > 0 ? band.min : MIN_NEIGHBOR_INTERVAL_SECS;
  const max = band?.max != null && band.max > 0 ? band.max : MAX_NEIGHBOR_INTERVAL_SECS;
  const n = Number(form.intervalSecs.trim());
  if (!Number.isInteger(n) || n < min || n > max) return { ok: false, min, max };
  return { ok: true, values: { enabled: form.enabled, interval_secs: n } };
}

/** Whether the form differs from what the server last reported — drives the Save button. */
export function isNeighborDirty(
  form: NeighborForm,
  saved: { enabled: boolean; interval_secs: number },
): boolean {
  return form.enabled !== saved.enabled || form.intervalSecs.trim() !== String(saved.interval_secs);
}

/** Build the form state from a server response. */
export function neighborFormFrom(saved: {
  enabled: boolean;
  interval_secs: number;
}): NeighborForm {
  return { enabled: saved.enabled, intervalSecs: String(saved.interval_secs) };
}

/** The cadence rendered for a human: seconds are what the API speaks, but "3600" is not a duration
 *  anyone reads at a glance. Whole hours and whole minutes get their own form; anything else stays
 *  in seconds rather than being rounded into a lie. */
export function describeCadence(
  secs: number,
  t: (key: string, opts?: Record<string, unknown>) => string,
): string {
  if (!Number.isFinite(secs) || secs <= 0) return t('settings.neighbors.cadence.seconds', { n: 0 });
  if (secs % 3600 === 0) return t('settings.neighbors.cadence.hours', { n: secs / 3600 });
  if (secs % 60 === 0) return t('settings.neighbors.cadence.minutes', { n: secs / 60 });
  return t('settings.neighbors.cadence.seconds', { n: secs });
}
