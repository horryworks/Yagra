// SPDX-License-Identifier: AGPL-3.0-only
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { BADGE_BRANDS, BRAND_MONOGRAMS, brandBadgeClass } from './brandBadge';
import { GROUP_ORIGIN_BADGE_BRANDS } from './groupOrigin';
import { NODE_KIND_SPEC } from './nodeKind';

const SRC = join(__dirname, '..');
/** The two badges, the stylesheet each one lives in, and the tokens its brand rule must read: the
 *  node header's pill wears the logo pair, the tree's one-letter chip the per-theme ink
 *  (2026-09-29). */
const PILLS: [string, string, (brand: string) => string[]][] = [
  ['.nd-kind', 'components/NodeDetail/NodeDetail.css', (b) => [`--brand-${b}`, `--brand-${b}-fg`]],
  ['.ntree-badge', 'components/NodeTree/NodeTree.css', (b) => [`--brand-${b}-ink`]],
];

describe('brand badges', () => {
  it('adds a class only for a brand', () => {
    expect(brandBadgeClass(null)).toBe('');
    expect(brandBadgeClass('meraki')).toBe(' is-meraki');
    expect(brandBadgeClass('netbox')).toBe(' is-netbox');
  });

  it('puts both Meraki badges in Meraki colours, the NetBox folder in NetBox colours, and nothing else', () => {
    expect(NODE_KIND_SPEC.meraki.badgeBrand).toBe('meraki');
    expect(GROUP_ORIGIN_BADGE_BRANDS.meraki).toBe('meraki');
    expect(GROUP_ORIGIN_BADGE_BRANDS.netbox).toBe('netbox');
    const others = [
      ...Object.entries(NODE_KIND_SPEC)
        .filter(([k]) => k !== 'meraki')
        .map(([, s]) => s.badgeBrand),
      ...Object.entries(GROUP_ORIGIN_BADGE_BRANDS)
        .filter(([k]) => k !== 'meraki' && k !== 'netbox')
        .map(([, b]) => b),
    ];
    expect(others.every((b) => b === null)).toBe(true);
  });

  // A brand named by either registry with no rule in a stylesheet draws the default accent — the
  // badge looks deliberate and is simply wrong, which no other test here can see.
  it('has a rule in both badge stylesheets for every brand, reading its tokens', () => {
    const tokens = readFileSync(join(SRC, 'styles/tokens.css'), 'utf8');
    // The ink is text on the page's own ground, so it is set once per theme; a brand with no dark
    // value would keep the light theme's dark ink on a dark row.
    const dark = tokens.slice(tokens.indexOf("[data-theme='dark']"));
    let checked = 0;
    for (const brand of BADGE_BRANDS) {
      expect(tokens, `--brand-${brand}`).toMatch(new RegExp(`--brand-${brand}:\\s*#`));
      expect(tokens, `--brand-${brand}-fg`).toMatch(new RegExp(`--brand-${brand}-fg:\\s*#`));
      expect(tokens, `--brand-${brand}-ink`).toMatch(new RegExp(`--brand-${brand}-ink:\\s*#`));
      expect(dark, `dark --brand-${brand}-ink`).toMatch(new RegExp(`--brand-${brand}-ink:\\s*#`));
      for (const [pill, file, reads] of PILLS) {
        const css = readFileSync(join(SRC, file), 'utf8');
        const rule = new RegExp(`\\${pill}\\.is-${brand}\\s*\\{([^}]*)\\}`).exec(css);
        expect(rule, `${pill}.is-${brand} in ${file}`).not.toBeNull();
        for (const token of reads(brand)) expect(rule?.[1]).toContain(`var(${token})`);
        checked++;
      }
    }
    expect(checked).toBe(BADGE_BRANDS.length * PILLS.length);
  });

  // The tree tells brands apart by this letter alone; two brands sharing one would be one badge.
  it('gives every brand one letter of its own', () => {
    const letters = BADGE_BRANDS.map((b) => BRAND_MONOGRAMS[b]);
    for (const l of letters) expect(l).toMatch(/^[A-Z]$/);
    expect(new Set(letters).size).toBe(letters.length);
  });
});
