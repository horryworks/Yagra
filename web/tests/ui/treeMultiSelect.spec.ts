// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree's working set — Ctrl / Shift multi-select (ADR-124, fixed in 増分 1).
//
// Both defects this file pins were reported from the running box, and neither was reachable by the
// suites that were green at the time:
//
//  1. **The Shift range never started.** The judgement lived in `NodeTree.tsx`, which Vitest never
//     loads, and the unit tests handed `rangeChecked` an anchor themselves — so the one branch that
//     was wrong (a plain click setting no anchor) was the one branch nothing ran. That half now has
//     a unit test as well, in `nodeTreeSelect.test.ts`; this one proves the gesture survives the
//     trip through the component, the two React state writes and the virtualized rows.
//  2. **The mark was painted in the hover colour.** `.ntree-row.checked` and `.ntree-row:hover`
//     were both `var(--bg-secondary)`, so a checked row was indistinguishable from a hovered one
//     and nearly indistinguishable from an idle one. Nothing can see that but a browser reading
//     computed styles: the class was on the element, the rule was in the stylesheet, and every
//     assertion anyone would write about either one passed.
//
// 🚨 Colour is read with `getComputedStyle`, never `isVisible()` — Playwright counts a mark nobody
// can see as visible, which is the whole shape of defect 2.

import { expect, test } from '../support/app';

/** The painted background of a row, as the operator's eye gets it. */
const bg = (loc: import('@playwright/test').Locator) =>
  loc.evaluate((el) => getComputedStyle(el).backgroundColor);

/** Park the pointer off every row, so a background read is the row's resting colour. */
async function unhover(page: import('@playwright/test').Page) {
  await page.mouse.move(2, 2);
}

test('a plain click then a Shift click takes the whole run, ends included', async ({ page }) => {
  // 🚨 THE REGRESSION, in the gesture the report used: click one row, Shift-click a row further
  // down, and expect everything between them. Before 増分 1 this checked exactly one row — the
  // second one — because the first click left no anchor and the Shift click fell through to
  // `rangeChecked`'s new-run branch.
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  await rows.nth(0).click();
  await rows.nth(2).click({ modifiers: ['Shift'] });

  await expect(page.locator('.ntree-row.checked')).toHaveCount(3);
  // The row in the middle is the one the operator said was skipped. Name it, so a future failure
  // says "the middle row again" rather than "expected 3, got 2".
  await expect(rows.nth(1)).toHaveClass(/checked/);

  // ADR-124 決定 2: the pane's selection stays single, and `.sel` keeps meaning what
  // `treeDeselect.spec.ts` counts it for.
  await expect(page.locator('.ntree-row.sel')).toHaveCount(1);
});

test('a checked row is painted in neither the idle nor the hover colour', async ({ page }) => {
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  // A folder row: never checked, never selected here, so it carries the tree's resting colour and
  // — while hovered — the hover colour. Reading both from one element keeps the comparison honest.
  const folder = page.locator('.ntree-grow').first();
  await expect(folder).toBeVisible();
  await unhover(page);
  const idle = await bg(folder);
  await folder.hover();
  const hover = await bg(folder);
  expect(hover, 'the tree has no hover colour to distinguish a mark from').not.toBe(idle);

  await rows.nth(0).click();
  await rows.nth(2).click({ modifiers: ['Shift'] });
  await unhover(page);

  // Row 1: checked, not the pane's selection, not hovered — the plain "in the batch" appearance.
  const checked = await bg(rows.nth(1));
  expect(checked, 'a checked row looks exactly like an unchecked one').not.toBe(idle);
  expect(checked, 'a checked row is painted in the hover colour, so it marks nothing').not.toBe(
    hover,
  );
});

test('Ctrl click adds a row to the batch without moving the pane', async ({ page }) => {
  // The half that already worked, pinned so the fix to Shift cannot take it away: Ctrl never
  // writes `?sel=`, which is what keeps ADR-073's clear-the-selection gestures untouched.
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  await rows.nth(0).click();
  const selected = new URL(page.url()).searchParams.get('sel');
  expect(selected).toMatch(/^node:/);

  await rows.nth(2).click({ modifiers: ['ControlOrMeta'] });
  // Two: the row the pane is showing and the row just Ctrl-clicked. This assertion said ONE until
  // 増分 3 — it was pinning the defect, which is what a test written from the implementation does.
  await expect(page.locator('.ntree-row.checked')).toHaveCount(2);
  expect(new URL(page.url()).searchParams.get('sel'), 'Ctrl click moved the pane').toBe(selected);
});

test('a plain click then Ctrl clicks keep the first row in the batch', async ({ page }) => {
  // 🚨 THE REGRESSION (Inc.3), in the gesture the report used: click sim-comware, then Ctrl-click
  // sim-huawei-vrp and sim-junos-vmx. The screenshot showed three marked rows — one accent bar,
  // two tints — and only the two Ctrl-clicked ones would have moved, because the plain click
  // emptied the batch and the Ctrl clicks started from nothing.
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  await rows.nth(0).click();
  await rows.nth(1).click({ modifiers: ['ControlOrMeta'] });
  await rows.nth(2).click({ modifiers: ['ControlOrMeta'] });

  await expect(page.locator('.ntree-row.checked')).toHaveCount(3);
  // Name the row that was dropped, so a future failure says "the first row again" rather than
  // "expected 3, got 2".
  await expect(rows.nth(0), 'the row the plain click landed on is not in the batch').toHaveClass(
    /checked/,
  );

  // And the batch is what actually moves: the modal lists all three, not the two Ctrl-clicked.
  await rows.nth(1).click({ button: 'right' });
  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();
  await menu.getByRole('button', { name: 'Move 3 selected…', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.movenode-list li')).toHaveCount(3);
});

test('Ctrl-clicking the row the pane shows takes it out of the batch', async ({ page }) => {
  // The other half of the rule: the pane's row STARTS the batch, so Ctrl-clicking it is how the
  // operator says "not that one" — and the pane keeps showing it either way.
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  await rows.nth(0).click();
  const selected = new URL(page.url()).searchParams.get('sel');
  await rows.nth(2).click({ modifiers: ['ControlOrMeta'] });
  await expect(page.locator('.ntree-row.checked')).toHaveCount(2);

  await rows.nth(0).click({ modifiers: ['ControlOrMeta'] });
  await expect(page.locator('.ntree-row.checked')).toHaveCount(1);
  await expect(rows.nth(2)).toHaveClass(/checked/);
  expect(new URL(page.url()).searchParams.get('sel'), 'the pane moved').toBe(selected);
});

test('a plain click abandons the batch', async ({ page }) => {
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  await rows.nth(0).click();
  await rows.nth(2).click({ modifiers: ['Shift'] });
  await expect(page.locator('.ntree-row.checked')).toHaveCount(3);

  await rows.nth(1).click();
  await expect(page.locator('.ntree-row.checked')).toHaveCount(0);
});

test('the menu on a checked row moves the whole batch, and offers nothing that moves one', async ({
  page,
}) => {
  // 🚨 THE REGRESSION (Inc.2), in the gesture the report used: Shift-select three rows, right-click
  // one of them, take the first "Move" you see. The menu used to put "Move to group…" — this one
  // row — above "Move 3 selected…", and on a menu that ran off the bottom of the screen the single
  // one was the only one visible. Ctrl and Shift were identical.
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  await rows.nth(0).click();
  await rows.nth(2).click({ modifiers: ['Shift'] });
  await expect(page.locator('.ntree-row.checked')).toHaveCount(3);

  await rows.nth(1).click({ button: 'right' });
  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();
  await expect(menu.getByRole('button', { name: 'Move to group…', exact: true })).toHaveCount(0);
  const bulk = menu.getByRole('button', { name: 'Move 3 selected…', exact: true });
  await expect(bulk).toBeVisible();
  // And it sits where the single item used to — the first move item in the menu, not below a
  // separator at the end, which is the position that put it below the fold.
  const labels = await menu.locator('button').allInnerTexts();
  const first = labels.find((l) => l.startsWith('Move'));
  expect(first, 'the first move item is not the batch one').toBe('Move 3 selected…');

  await bulk.click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.movenode-list li')).toHaveCount(3);
});

test('the menu on a row outside the batch names the row, and still offers the batch', async ({
  page,
}) => {
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  await rows.nth(0).click({ modifiers: ['ControlOrMeta'] });
  await rows.nth(1).click({ modifiers: ['ControlOrMeta'] });
  await expect(page.locator('.ntree-row.checked')).toHaveCount(2);

  const name = await rows.nth(2).locator('.ntree-node-name').innerText();
  await rows.nth(2).click({ button: 'right' });
  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();
  // ADR-055 R1: with a batch on screen, an unqualified "Move to group…" reads as the batch.
  await expect(menu.getByRole('button', { name: 'Move to group…', exact: true })).toHaveCount(0);
  await expect(
    menu.getByRole('button', { name: `Move "${name}" to group…`, exact: true }),
  ).toBeVisible();
  await expect(menu.getByRole('button', { name: 'Move 2 selected…', exact: true })).toBeVisible();
});

test('the hover ↗ on a checked row moves the batch', async ({ page }) => {
  // The same rule decides the row's own move button; a second copy of it would be the next bug.
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  await rows.nth(0).click();
  await rows.nth(2).click({ modifiers: ['Shift'] });
  await expect(page.locator('.ntree-row.checked')).toHaveCount(3);

  await rows.nth(1).hover();
  const act = rows.nth(1).locator('.ntree-act');
  await expect(act).toHaveAttribute('title', 'Move 3 selected…');
  await act.click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.movenode-list li')).toHaveCount(3);
});
