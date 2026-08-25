// SPDX-License-Identifier: AGPL-3.0-only
// Brand mark geometry (§1.1) — the topology fork that also reads as a "Y": one root node
// branching to two. Adopted from the Yagra-Website brand assets so the product and the site
// carry the same mark; change it in both repos or they drift.
//
// Single source for the two renderings that must agree:
//   - <Logo> (this directory) — the in-app seal/mark, 24–56px.
//   - web/public/favicon.svg — a static file, so it cannot import this one; brandMark.test.ts
//     is what pins it here (extensibility.md §2).

// ⚠️ These two are the SECOND copy of a value `styles/tokens.css` also declares
// (`--brand-fixed` / `--brand-mark`). Both copies are needed — the <Logo> emits SVG *attributes*,
// and `favicon.svg` can import neither file — so what stops them drifting is a test, and it is
// **not in this directory**: `web/tests/ui/brandColors.spec.ts` compares the token the browser
// resolves against the colour the Logo actually paints. It cannot live in Vitest, which stubs CSS
// imports to an empty string even with `?raw` (measured three ways). Change one, change both, and
// run `npm run build && npm run test:ui`.
export const BRAND = '#e95d08'; // fixed brand orange (--brand-fixed)
export const MARK = '#faf7f3'; // off-white 生成り mark (--brand-mark)

/** Every coordinate below is in this viewBox. */
export const MARK_VIEWBOX = '0 0 64 64';

/** The rounded seal tile the mark sits on (omitted by the 'mark' variant). */
export const SEAL_TILE = { x: 1, y: 1, size: 62, rx: 13 } as const;

/** The three nodes: the root at the bottom and the two it branches to. */
export const MARK_NODES = [
  { cx: 32, cy: 48.5 },
  { cx: 19, cy: 18.5 },
  { cx: 45, cy: 18.5 },
] as const;

/** The edges between them. The ends stop inside the node circles on purpose. */
export const MARK_EDGES = ['M32 46 V33', 'M32 33 L20 20', 'M32 33 L44 20'] as const;

/**
 * Two weights of the same mark. The favicon renders down at 16px, where the lighter strokes
 * close up and the nodes lose their gap — so it is drawn slightly bolder. The website makes
 * the same split between its header logo and its favicon.
 */
export const MARK_WEIGHTS = {
  logo: { stroke: 4.5, node: 5 },
  favicon: { stroke: 5, node: 5.5 },
} as const;
