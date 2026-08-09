// SPDX-License-Identifier: AGPL-3.0-only
// Displaying the operator-named values a URL monitor lifts out of a JSON body (ADR-047 Inc.3).
//
// In a `.ts` because Vitest never runs `.tsx`: the rounding below is a judgement (how much
// precision is honest for a number whose units nobody declared), and a judgement in a `.tsx` is a
// judgement nothing tests.

/** Separator for {@link extractMetricKey} / {@link metricsFromKey}. A comma is illegal in a metric
 *  name (they are `[a-z0-9_]`), so no name can be split in half by it. */
const KEY_SEP = ',';

/** Collapse a rule list into one stable string, for use as a `useEffect` dependency.
 *
 *  The rules arrive as a fresh array on every render, so depending on the array itself re-runs the
 *  effect forever; depending on this string re-runs it only when the *names* change.
 *
 *  Paired with {@link metricsFromKey} on purpose. The two were once written independently — the
 *  join used a literal NUL and the split used a space — and the disagreement was invisible for a
 *  monitor with a single rule (a one-element join has no separator in it at all) while silently
 *  dropping **every** extracted value on a monitor with two or more. */
export function extractMetricKey(extracts: readonly { metric: string }[]): string {
  return extracts.map((e) => e.metric).join(KEY_SEP);
}

/** The metric names in a key built by {@link extractMetricKey} (empty key ⇒ no rules). */
export function metricsFromKey(key: string): string[] {
  return key === '' ? [] : key.split(KEY_SEP);
}

/** Format an extracted value for a health card.
 *
 *  These are arbitrary numbers from someone else's API — a queue depth, a ratio, a byte count —
 *  with **no declared unit and no declared precision**, so the rule has to work for all of them:
 *  keep whole numbers whole, and round the rest to a few decimals rather than printing the float's
 *  full expansion. `0.1 + 0.2` arriving as `0.30000000000000004` is not a reading anyone wants to
 *  read off a dashboard, and trailing zeros imply a precision the source never claimed. */
export function formatExtractedValue(v: number): string {
  if (!Number.isFinite(v)) return '—';
  if (Number.isInteger(v)) return v.toLocaleString();
  // Three decimals, then strip what the rounding left behind (1.500 → 1.5). A magnitude small
  // enough to round away entirely keeps its exponent instead of collapsing to a misleading "0".
  const rounded = Number(v.toFixed(3));
  if (rounded === 0) return v.toExponential(2);
  return rounded.toLocaleString(undefined, { maximumFractionDigits: 3 });
}
