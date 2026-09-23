// SPDX-License-Identifier: AGPL-3.0-only
// A badge that names a third party wears that party's colours (2026-09-23, ADR-164 増分 14).
//
// Two registries point here — `NODE_KIND_SPEC[kind].badgeBrand` and `GROUP_ORIGIN_BADGE_BRANDS`
// — and two stylesheets carry the look, one per badge (`.nd-kind` in NodeDetail.css,
// `.ntree-badge` in NodeTree.css; a component owns its own CSS file). `brandBadge.test.ts` holds
// the three together: a brand either registry names must have its rule in both stylesheets, or the
// badge silently keeps the default accent.
//
// The colours themselves are tokens (`--brand-<name>` / `--brand-<name>-fg` in tokens.css).

/** The third parties whose colours a badge may wear. */
export const BADGE_BRANDS = ['meraki', 'netbox'] as const;
export type BadgeBrand = (typeof BADGE_BRANDS)[number];

/** The class that puts a badge in `brand`'s colours, or `''` for Yagra's own accent. */
export function brandBadgeClass(brand: BadgeBrand | null): string {
  return brand ? ` is-${brand}` : '';
}
