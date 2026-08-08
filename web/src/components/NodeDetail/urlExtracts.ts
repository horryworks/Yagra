// SPDX-License-Identifier: AGPL-3.0-only
// Displaying the operator-named values a URL monitor lifts out of a JSON body (ADR-047 Inc.3).
//
// In a `.ts` because Vitest never runs `.tsx`: the rounding below is a judgement (how much
// precision is honest for a number whose units nobody declared), and a judgement in a `.tsx` is a
// judgement nothing tests.

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
