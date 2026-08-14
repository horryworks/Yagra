// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree's row menu — the regression jsdom cannot reach (ADR-052 決定 7, 出典 3).
//
// TWO SHIPPED BUGS LIVE HERE, and both are invisible without a layout engine:
//
//   `portal the action menu so a virtualized row cannot capture it` — a `position: fixed` panel
//   resolves against the nearest ancestor that establishes a containing block, and a virtualized
//   row's `transform: translateY(…)` is exactly that. The menu laid itself out off screen; the
//   only visible symptom was a stray horizontal scrollbar. Nothing about the DOM was wrong, so no
//   DOM-shape assertion could have caught it — only "where did this actually end up".
//
//   `stop a tree row's + menu closing in the frame it opens` — the click that opens the menu also
//   reaches the outside-click handler that closes it. A test that asserts the menu appears will
//   pass on the frame it appears in; the assertion has to survive the frame after.
//
// Both need many rows (virtualization only engages past a screenful) and a scrolled viewport,
// which is the other half of why this is Tier1 work: on a real deployment the inventory is
// whatever it is, and "scroll to row 40" is not a thing a test can arrange.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const GROUP_COUNT = 60;

/** The generated group row, repeated into a flat list long enough to virtualize. Built from the
 *  generated shape rather than hand-written, so a change to `GroupSummary` reaches this too. */
function manyGroups(): Json {
  const [template] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  return Array.from({ length: GROUP_COUNT }, (_, i) => ({
    ...template,
    id: `00000000-0000-4000-8000-${String(i).padStart(12, '0')}`,
    name: `${MOCK_PREFIX}group-${String(i).padStart(2, '0')}`,
    parent_id: null,
  })) as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/node-groups': manyGroups() },
  },
});

test('a row menu opens inside the viewport, however far down the row is', async ({ page }) => {
  await page.goto('/nodes');
  // Group rows only () — a node row has no ＋, and the last row in the tree is one.
  const rows = page.locator('.ntree-grow');
  await expect(rows.first()).toBeVisible();

  // Scroll the tree to its end. This is the whole point: the rows now on screen carry a large
  // `translateY`, which is what used to become the menu's containing block.
  //
  // ⚠️ Two things this cannot do. It cannot scroll `rows.last()` into view — under virtualization
  // that is the last *rendered* row, already near the fold, and the locator re-resolves to a
  // different element after every re-render, so a hover and the click after it can land on two
  // different rows. And it cannot jump straight to `scrollHeight`: the window only re-renders per
  // scroll event, so the target row has to be walked to.
  const anchor = rows.filter({ hasText: `${MOCK_PREFIX}group-${GROUP_COUNT - 1}` });
  const scroller = page.locator('.ntree-body');
  for (let step = 0; step < 40 && (await anchor.count()) === 0; step++) {
    await scroller.evaluate((el) => {
      el.scrollTop += el.clientHeight;
    });
    await page.waitForTimeout(30);
  }
  await expect(anchor, 'never reached the last group by scrolling').toBeVisible();

  // The precondition, asserted rather than assumed. Everything below is only a test of the portal
  // if this row actually sits inside a large `translateY` — otherwise the menu would land in the
  // right place whether or not the portal existed, and the whole file would pass vacuously.
  const offset = await anchor.evaluate((el) => {
    const wrapper = el.parentElement as HTMLElement | null;
    return Number(/translateY\((-?[\d.]+)px\)/.exec(wrapper?.style.transform ?? '')?.[1] ?? 0);
  });
  expect(offset, 'the anchor row carries no virtualization transform to be captured by').toBeGreaterThan(
    1000,
  );

  // The ＋ is hover-revealed, so hovering is part of the interaction, not test scaffolding.
  await anchor.hover();
  const trigger = anchor.getByRole('button', { name: 'Add' });
  await trigger.click();

  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();

  const box = await menu.boundingBox();
  const viewport = page.viewportSize();
  expect(box, 'the menu has no layout box at all').not.toBeNull();
  expect(viewport).not.toBeNull();
  if (!box || !viewport) return;

  // The assertion the portal exists to make true. `toBeVisible()` alone does NOT cover it —
  // an element positioned off to the right is still "visible" to the DOM.
  expect(box.x, 'menu is off the left edge').toBeGreaterThanOrEqual(0);
  expect(box.y, 'menu is off the top edge').toBeGreaterThanOrEqual(0);
  expect(box.x + box.width, 'menu overflows the right edge').toBeLessThanOrEqual(viewport.width);
  expect(box.y + box.height, 'menu overflows the bottom edge').toBeLessThanOrEqual(viewport.height);

  // And nothing may have grown a horizontal scrollbar — the original symptom, and a second,
  // independent witness that the panel landed somewhere real.
  const overflows = await page.evaluate(
    () => document.documentElement.scrollWidth > document.documentElement.clientWidth,
  );
  expect(overflows, 'the page scrolls horizontally — something is laid out off to the side').toBe(
    false,
  );
});

test('a row menu stays open in the frame after it opens', async ({ page }) => {
  await page.goto('/nodes');
  const row = page.locator('.ntree-grow').first();
  await expect(row).toBeVisible();
  await row.hover();
  await row.getByRole('button', { name: 'Add' }).click();

  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();
  // Two animation frames after the opening click: long enough for a mousedown/click handler that
  // fires on the document to have closed it again.
  await page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r(null)))),
  );
  await expect(menu, 'the menu closed itself in the frame it opened').toBeVisible();
});

test('Escape closes the menu and returns focus to the trigger', async ({ page }) => {
  await page.goto('/nodes');
  const row = page.locator('.ntree-grow').first();
  await expect(row).toBeVisible();
  await row.hover();
  const trigger = row.getByRole('button', { name: 'Add' });
  await trigger.click();
  await expect(page.getByRole('menu')).toBeVisible();

  await page.keyboard.press('Escape');
  await expect(page.getByRole('menu')).toHaveCount(0);
  // `ui-conventions.md` requires primary flows stay keyboard-operable; a popover that drops focus
  // on the body strands a keyboard user wherever the document happens to start.
  await expect(trigger).toBeFocused();
});
