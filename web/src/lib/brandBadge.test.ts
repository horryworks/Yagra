// SPDX-License-Identifier: AGPL-3.0-only
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { BADGE_BRANDS, brandBadgeClass } from './brandBadge';
import { GROUP_ORIGIN_BADGE_BRANDS } from './groupOrigin';
import { NODE_KIND_SPEC } from './nodeKind';

const SRC = join(__dirname, '..');
/** The two pill badges and the stylesheet each one lives in. */
const PILLS: [string, string][] = [
  ['.nd-kind', 'components/NodeDetail/NodeDetail.css'],
  ['.ntree-badge', 'components/NodeTree/NodeTree.css'],
];

describe('brand badges', () => {
  it('adds a class only for a brand', () => {
    expect(brandBadgeClass(null)).toBe('');
    expect(brandBadgeClass('meraki')).toBe(' is-meraki');
  });

  it('puts both Meraki badges in Meraki colours, and nothing else', () => {
    expect(NODE_KIND_SPEC.meraki.badgeBrand).toBe('meraki');
    expect(GROUP_ORIGIN_BADGE_BRANDS.meraki).toBe('meraki');
    const others = [
      ...Object.entries(NODE_KIND_SPEC)
        .filter(([k]) => k !== 'meraki')
        .map(([, s]) => s.badgeBrand),
      ...Object.entries(GROUP_ORIGIN_BADGE_BRANDS)
        .filter(([k]) => k !== 'meraki')
        .map(([, b]) => b),
    ];
    expect(others.every((b) => b === null)).toBe(true);
  });

  // A brand named by either registry with no rule in a stylesheet draws the default accent — the
  // badge looks deliberate and is simply wrong, which no other test here can see.
  it('has a rule in both badge stylesheets for every brand, reading its two tokens', () => {
    const tokens = readFileSync(join(SRC, 'styles/tokens.css'), 'utf8');
    let checked = 0;
    for (const brand of BADGE_BRANDS) {
      expect(tokens, `--brand-${brand}`).toMatch(new RegExp(`--brand-${brand}:\\s*#`));
      expect(tokens, `--brand-${brand}-fg`).toMatch(new RegExp(`--brand-${brand}-fg:\\s*#`));
      for (const [pill, file] of PILLS) {
        const css = readFileSync(join(SRC, file), 'utf8');
        const rule = new RegExp(`\\${pill}\\.is-${brand}\\s*\\{([^}]*)\\}`).exec(css);
        expect(rule, `${pill}.is-${brand} in ${file}`).not.toBeNull();
        expect(rule?.[1]).toContain(`var(--brand-${brand})`);
        expect(rule?.[1]).toContain(`var(--brand-${brand}-fg)`);
        checked++;
      }
    }
    expect(checked).toBe(BADGE_BRANDS.length * PILLS.length);
  });
});
