// SPDX-License-Identifier: AGPL-3.0-only
// The two decisions `MetricChart` makes about what a series looks like, kept where a test can run
// them (`environment: 'node'` + `include: ['src/**/*.test.ts']`, see testing.md).
//
// Both exist because **uPlot draws on a canvas and a canvas cannot read a CSS variable.** Every
// other surface in this app takes its colours from `tokens.css` through `var(--…)`; the chart has
// to resolve them to literals first, which is the one place a theme token can silently become the
// wrong colour rather than no colour.

import type uPlot from 'uplot';
import { idleLegendIdx } from './legend';

/**
 * A series colour as a literal the canvas can use.
 *
 * Three cases, and the order matters:
 *
 * 1. nothing given ⇒ the caller's fallback (the palette entry for this series index);
 * 2. a literal (`#0af`, `rgb(...)`, a palette constant) ⇒ used as-is, never looked up;
 * 3. exactly `var(--name)` ⇒ resolved against the passed computed style.
 *
 * ⚠️ A `var()` that resolves to nothing falls back rather than painting an empty string, which
 * canvas renders as **black** — the failure this guards against is not an error, it is a chart
 * that looks deliberate and is wrong on one theme only.
 */
export function resolveColor(
  raw: string | undefined,
  cs: CSSStyleDeclaration,
  fallback: string,
): string {
  if (!raw) return fallback;
  const m = raw.match(/^var\((--[\w-]+)\)$/);
  if (!m) return raw;
  return cs.getPropertyValue(m[1]).trim() || fallback;
}

/**
 * Point the legend at the most recent usable sample while the cursor is idle.
 *
 * ⚠️ **Only while idle.** `u.cursor.idx != null` means the operator is hovering a point, and
 * overwriting the legend then would fight their pointer — the reading under the cursor would flick
 * back to the latest sample every time the chart re-drew.
 *
 * The first data row is the x axis, hence `slice(1)`.
 */
export function applyIdleLegend(u: uPlot) {
  if (u.cursor.idx != null) return;
  const idx = idleLegendIdx(u.data.slice(1) as (number | null)[][]);
  if (idx != null) u.setLegend({ idx }, false);
}
