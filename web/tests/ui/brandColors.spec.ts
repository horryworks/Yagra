// SPDX-License-Identifier: AGPL-3.0-only
// The two fixed brand colours exist in two places by necessity, and nothing compared them.
//
// `brandMark.ts` holds them as literals because the <Logo> emits SVG *attributes* (`fill=`,
// `stroke=`) rather than classes, and `public/favicon.svg` is a static file that can import
// neither. `styles/tokens.css` holds them as `--brand-fixed` / `--brand-mark`, which is what every
// stylesheet reads. `brandMark.test.ts` pins favicon.svg to `brandMark.ts` — so the SVG side is
// guarded — but no test has ever read `tokens.css`, and editing it alone leaves the browser-tab
// icon and the in-app seal on the OLD colour with all 2,495 Vitest tests still green.
//
// 🚨 **This check cannot live in Vitest.** Vitest stubs CSS imports to an empty string, and it does
// so even for `?raw` and even with `test.css.include` set (all three measured). A test written
// there reads `''`, finds no declaration, and the only reason it fails is the length assertion —
// exactly the "looked at nothing" failure `testing.md` warns about. `node:fs` is not the way out
// either: `src/**` is typechecked by `npm run build`, and the WebUI deliberately has no
// `@types/node`.
//
// So it lives here, where a real browser has both halves — and that makes it a *stronger* check
// than comparing two files' text. It compares the custom property **in effect** against the colour
// the Logo **actually paints**, so a theme override, a specificity accident or a shadowed
// declaration fails it too, none of which a text diff can see.

import { expect, test } from '../support/app';

/** The custom property as the browser resolves it. `getPropertyValue` on a custom property returns
 *  the declared token text (`#e95d08`), not a computed `rgb()`, so it compares to the SVG attribute
 *  directly. Lower-cased and trimmed because CSS and the SVG attribute are free to differ in case. */
async function token(page: import('@playwright/test').Page, name: string): Promise<string> {
  const value = await page.evaluate(
    (n) => getComputedStyle(document.documentElement).getPropertyValue(n),
    name,
  );
  return value.trim().toLowerCase();
}

test.describe('the brand tokens and the mark the app paints agree', () => {
  test('the seal tile is the colour --brand-fixed declares', async ({ page }) => {
    await page.goto('/dashboard');

    // The top bar's <Logo> defaults to the 'seal' variant, so the tile <rect> is present.
    const logo = page.getByRole('img', { name: 'Yagra' }).first();
    await expect(logo).toBeAttached();

    // 🚨 Read the positive side FIRST and assert it is non-empty. Both halves of this comparison
    // can come back as '' — an absent element yields null→'' and a missing custom property yields
    // '' — and '' === '' would make a screen with no logo at all look like agreement.
    const painted = (await logo.locator('rect').first().getAttribute('fill'))?.trim().toLowerCase();
    expect(painted).toMatch(/^#[0-9a-f]{6}$/);

    const declared = await token(page, '--brand-fixed');
    expect(declared).toMatch(/^#[0-9a-f]{6}$/);

    expect(painted).toBe(declared);
  });

  test('the mark is the colour --brand-mark declares', async ({ page }) => {
    await page.goto('/dashboard');

    const logo = page.getByRole('img', { name: 'Yagra' }).first();
    await expect(logo).toBeAttached();

    // The edges are stroked and the nodes are filled, both with MARK — check both, because a
    // change that touched only one would leave the mark two-toned.
    // ⚠️ `g[fill]` alone is wrong and it fails loudly rather than quietly: the *stroke* group also
    // carries `fill="none"` to keep the paths open, so a bare `.first()` reads "none". Ask for the
    // group that fills and does not stroke.
    const stroked = (await logo.locator('g[stroke]').first().getAttribute('stroke'))
      ?.trim()
      .toLowerCase();
    const filled = (await logo.locator('g[fill]:not([stroke])').first().getAttribute('fill'))
      ?.trim()
      .toLowerCase();
    expect(stroked).toMatch(/^#[0-9a-f]{6}$/);
    expect(filled).toMatch(/^#[0-9a-f]{6}$/);

    const declared = await token(page, '--brand-mark');
    expect(declared).toMatch(/^#[0-9a-f]{6}$/);

    expect(stroked).toBe(declared);
    expect(filled).toBe(declared);
  });
});
