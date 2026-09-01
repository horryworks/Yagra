// SPDX-License-Identifier: AGPL-3.0-only
// How a finding becomes a colour.
//
// Three report bodies each had their own `sevColor`, byte-identical apart from the colour of the
// `info` tier (`--series-5` / `--series-1` / `--series-6`), and a fourth had the same three-way
// mapping under the name `toneColor`. That is `extensibility.md` §3 exactly — if two functions
// differ only by a value, pass the value — and the copies sat in `.tsx` files, where no test could
// reach any of them.
//
// ⚠️ **The `info` colour is deliberately a parameter with no default.** It is not decoration: each
// body picks the series colour that its own charts already use, so a shared default would quietly
// re-colour three reports the day someone added a fourth caller. Making it required means a new
// body has to answer the question rather than inherit an answer.
//
// `crit` and `warn` are NOT parameters. Those are the alert state machine's colours
// (`ui-conventions.md`: "a node's status colour must be identical in the table, the map, and the
// chart"), and a report that chose its own red would be the second answer.

import { capacityBucket, correlationDirection, sevOf } from './format';

/** The three-step scale every report body ranks on. Matches `FindingSeverity`'s three values. */
export const TONES = ['crit', 'warn', 'info'] as const;
export type Tone = (typeof TONES)[number];

/**
 * A tone as a CSS colour.
 *
 * @param info the colour for the lowest tier — see the module header for why it has no default.
 */
export function toneColor(tone: Tone, info: string): string {
  if (tone === 'crit') return 'var(--status-critical)';
  if (tone === 'warn') return 'var(--status-warning)';
  return info;
}

/** A finding's own severity as a colour. The narrowing is `sevOf`'s, so an unknown severity reads
 *  as `info` here for the same reason it does everywhere else. */
export function severityColor(f: { severity: string }, info: string): string {
  return toneColor(sevOf(f), info);
}

/**
 * A 0–100 score as a tone.
 *
 * ⚠️ Mirrors the backend's `severity_for` thresholds (`analysis/stats.rs`). The grouped event-flap
 * rows have no `severity` string of their own — they are assembled in the browser from several
 * findings — so this is the only place their colour can come from.
 */
export function scoreTone(score: number): Tone {
  if (score >= 90) return 'crit';
  if (score >= 75) return 'warn';
  return 'info';
}

/** A talker's rank as a tone: the busiest talker is the finding, the next two are context. */
export function rankTone(rank: number): Tone {
  if (rank <= 1) return 'crit';
  if (rank <= 3) return 'warn';
  return 'info';
}

/** Time-to-exhaustion as a tone, via the bucket the filter row already uses — so the colour and
 *  the filter can never disagree about what "soon" is. */
export function capacityTone(tteDays: number): Tone {
  const b = capacityBucket(tteDays);
  if (b === 'soon') return 'crit';
  if (b === 'mid') return 'warn';
  return 'info';
}

/** Categorical colours for the passive-event source kinds.
 *
 *  ⚠️ These are **not statuses** — a trap is not worse than a syslog line — so they come from the
 *  series palette and an unknown kind falls back to tertiary text rather than to a status colour. */
const SOURCE_COLORS: Record<string, string> = {
  trap: 'var(--series-5)',
  syslog: 'var(--series-1)',
  webhook: 'var(--series-3)',
};

export function sourceColor(kind: string): string {
  return SOURCE_COLORS[kind] ?? 'var(--text-tertiary)';
}

/** A correlation's direction as a colour, via `correlationDirection` so the bar and the label
 *  cannot disagree about which side of zero `r` is on. */
export function correlationColor(r: number): string {
  return correlationDirection(r) === 'coRising' ? 'var(--series-1)' : 'var(--series-4)';
}
