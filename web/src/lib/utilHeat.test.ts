// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { utilHeat } from './utilHeat';

/** The wash percentage a result carries, as a number. */
function washOf(pct: number): number {
  const h = utilHeat(pct);
  if (!h) throw new Error(`expected a wash for ${pct}`);
  return Number.parseFloat(h.wash);
}

describe('utilHeat', () => {
  // ── the cases that produce nothing ──────────────────────────────────────────────────────────
  //
  // 🚨 These four alone would be satisfied by an implementation that returns `null` for
  // everything, which is why the block below asserts colours are actually produced
  // (`rejection-only-tests-pass-when-everything-rejects`).
  it("paints nothing when there is no reading to place on the ramp", () => {
    expect(utilHeat(null)).toBeNull();
    expect(utilHeat(undefined)).toBeNull();
    expect(utilHeat(Number.NaN)).toBeNull();
    expect(utilHeat(Number.POSITIVE_INFINITY)).toBeNull();
    // Negative would mean corrupt counter arithmetic; a colour derived from it would be a lie.
    expect(utilHeat(-1)).toBeNull();
  });

  // The distinction the API draws and the UI must not lose: a port that never advertised a rate
  // has no denominator (null), an idle port has one (0). They must not render the same.
  it('separates "no link rate known" from "the link is idle"', () => {
    expect(utilHeat(null)).toBeNull();
    expect(utilHeat(0)).not.toBeNull();
  });

  // ── the cases that produce a colour ─────────────────────────────────────────────────────────
  it("places 0 at the cool end and 100 at the hot end", () => {
    const idle = utilHeat(0)!;
    const saturated = utilHeat(100)!;
    // At 0 none of the mid colour survives, so the cell is the cool token.
    expect(idle.hue).toContain('var(--util-cool)');
    expect(idle.hue).toContain('0%');
    // At 100 the hot token has fully displaced the mid one.
    expect(saturated.hue).toContain('var(--util-hot)');
    expect(saturated.hue).toContain('100%');
  });

  it("turns over at 70%, where the second segment starts", () => {
    // Just under the stop: still on the cool→mid segment, and nearly all of the way along it.
    expect(utilHeat(69)!.hue).toContain('var(--util-cool)');
    // At and above it: on the mid→hot segment.
    expect(utilHeat(70)!.hue).toContain('var(--util-cool)');
    expect(utilHeat(71)!.hue).toContain('var(--util-hot)');
    // The stop itself is the seam — the first segment ends on the mid colour outright.
    expect(utilHeat(70)!.hue).toContain('100%');
  });

  it("deepens the wash monotonically, so the strength carries the reading too", () => {
    const samples = [0, 10, 25, 50, 70, 85, 100];
    const washes = samples.map(washOf);
    for (let i = 1; i < washes.length; i += 1) {
      expect(
        washes[i],
        `wash at ${samples[i]}% vs ${samples[i - 1]}%`,
      ).toBeGreaterThan(washes[i - 1]);
    }
    // Both ends are inside the declared band, and the band has real width — a wash that never
    // moved would satisfy "monotonic" if the comparison were >=.
    expect(washes[0]).toBeCloseTo(8, 5);
    expect(washes[washes.length - 1]).toBeCloseTo(30, 5);
  });

  it("clamps a reading above the advertised rate instead of refusing it", () => {
    // Devices do report counters that outrun their own ifSpeed. "More than full" is still the
    // top of the ramp, not an absence.
    expect(utilHeat(140)).toEqual(utilHeat(100));
  });

  // ── the theming rule, asserted rather than trusted ──────────────────────────────────────────
  it("names tokens and never a literal colour", () => {
    for (const pct of [0, 1, 35, 70, 99, 100]) {
      const h = utilHeat(pct)!;
      expect(h.hue).toContain('var(--util-');
      // `/verify`'s theming step looks for exactly this: a hex or an rgb() built in a component.
      expect(h.hue).not.toMatch(/#[0-9a-f]{3,8}\b/i);
      expect(h.hue).not.toMatch(/\brgba?\(/i);
    }
  });

  it("emits percentages a browser can parse", () => {
    for (const pct of [0, 1 / 3, 33.333333, 70.0000001, 99.9, 100]) {
      const h = utilHeat(pct)!;
      expect(h.wash).toMatch(/^\d+(\.\d)?%$/);
      // One decimal at most, so a float artefact cannot reach the style attribute.
      expect(h.hue).not.toMatch(/\d\.\d\d+%/);
    }
  });
});
