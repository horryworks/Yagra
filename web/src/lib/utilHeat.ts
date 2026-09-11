// SPDX-License-Identifier: AGPL-3.0-only
// Interface utilization → the background wash the Interfaces list paints behind a bps figure
// (ADR-126). Green when a link is quiet, red when it is close to its rate.
//
// Pure, and in a `.ts` on purpose: Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so a rule written inside `InterfacesTab.tsx` is a rule no test
// can reach — and `tsxJudgement.test.ts` fails the build for one. The same split `linkMode.ts`
// and `interfaceMetrics.ts` use.
//
// ⚠️ It lives in `lib/` rather than beside the tab because five places already decide a colour
// from a utilization figure (`dashboard/widgets/performance.tsx`, `sites.tsx`, and three
// `troubleshoot/report/bodies/`). Those are **not** folded in here — they pick one of three
// discrete bar colours where this returns a continuous wash, so the output shape differs and
// merging them would need a `toneColor`-style parameter neither side wants yet. What this
// placement buys is that folding them later is an edit, not a move.

/** Where the ramp turns from "quiet" toward "hot".
 *
 *  70 is not invented: `performance.tsx` already calls 70% the point a link stops being
 *  comfortable, and reusing it keeps one meaning for one number across two screens.
 *
 *  ⚠️ There is deliberately **no second stop at 90**. Leaving 90–100 as its own segment spends a
 *  third of the ramp's width on the range that matters most, so a genuinely saturated port would
 *  read as merely busy. With one stop, 70 → 100 runs the whole way to red. */
const HOT_FROM_PCT = 70;

/** How strongly the colour tints the row behind it, at 0% and at 100% utilization.
 *
 *  The strength ramps with the reading as well as the hue, so the two encode the same fact twice:
 *  an operator who cannot separate the hues still sees a faint cell beside a solid one
 *  (`ui-conventions.md`, "don't rely on color alone"). */
const WASH_MIN_PCT = 8;
const WASH_MAX_PCT = 30;

/** The two inline custom properties a heat-washed cell carries. `InterfacesTab` spreads these onto
 *  the cell's `style`; `NodeDetail.css` does the final `color-mix` against `transparent`. */
export interface UtilHeat {
  /** A CSS colour: the ramp position, as a `color-mix` of two `--util-*` tokens. */
  hue: string;
  /** A CSS percentage: how much of `hue` survives the mix with `transparent`. */
  wash: string;
}

/** Clamp to the ramp's domain, or `null` when there is no reading to place on it.
 *
 *  🚨 **`null` and `0` are different answers and must stay different.** The API returns `null` for
 *  an interface that never advertised a rate (`api/collection.rs` refuses to divide by an absent
 *  or zero speed), and painting such a cell would show a judgement with no denominator behind it.
 *  `0` is a real reading — an idle link — and gets the coolest end of the ramp.
 *
 *  A reading above 100 is clamped rather than rejected: a device's advertised `ifSpeed` and its
 *  own counters do disagree in practice, and "more than full" is still the top of the ramp. */
function rampPosition(pct: number | null | undefined): number | null {
  if (pct == null || !Number.isFinite(pct) || pct < 0) return null;
  return Math.min(100, pct);
}

/**
 * The wash for one direction's utilization, or `null` when the cell should stay unpainted.
 *
 * Returns CSS **strings** rather than resolved colours: the `--util-*` tokens are already
 * theme-switched (they alias `--status-*`, which `[data-theme='dark']` overrides), so building the
 * mix in CSS means light and dark need no second answer here. It also makes the result assertable
 * — a test can see that no hex was hardcoded, which is what `/verify`'s theming step looks for.
 */
export function utilHeat(pct: number | null | undefined): UtilHeat | null {
  const p = rampPosition(pct);
  if (p == null) return null;
  // Two segments, each mixed in oklab so the midpoint reads as a colour between the ends rather
  // than as the muddy sRGB average — the same space `Heatmap.tsx` mixes in.
  const hue =
    p <= HOT_FROM_PCT
      ? `color-mix(in oklab, var(--util-mid) ${pctText((p / HOT_FROM_PCT) * 100)}, var(--util-cool))`
      : `color-mix(in oklab, var(--util-hot) ${pctText(
          ((p - HOT_FROM_PCT) / (100 - HOT_FROM_PCT)) * 100,
        )}, var(--util-mid))`;
  const wash = WASH_MIN_PCT + (WASH_MAX_PCT - WASH_MIN_PCT) * (p / 100);
  return { hue, wash: pctText(wash) };
}

/** A percentage with at most one decimal and no trailing zero — `40%`, not `40.0000001%`. */
function pctText(v: number): string {
  return `${Math.round(v * 10) / 10}%`;
}
