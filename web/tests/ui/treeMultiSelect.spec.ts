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
  await expect(dialog.locator('.form-targets li')).toHaveCount(3);
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
  await expect(dialog.locator('.form-targets li')).toHaveCount(3);
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
  await expect(dialog.locator('.form-targets li')).toHaveCount(3);
});

test('a row-only action names its node while a batch is on screen', async ({ page }) => {
  // 🚨 ADR-124 増分 9. Edit and Pin act on the right-clicked row and have no batch form, but they
  // sat in a menu headed "Move 3 selected…" and said plainly "Edit node…" / "Pin" — so the menu
  // offered twenty-node verbs and one-node verbs in the same list with nothing to tell them apart.
  // This is the ADR-055 R1 rule the moves already follow, applied to the items that cannot switch.
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  await rows.nth(0).click();
  await rows.nth(2).click({ modifiers: ['Shift'] });
  await expect(page.locator('.ntree-row.checked')).toHaveCount(3);

  const name = await rows.nth(1).locator('.ntree-node-name').innerText();
  await rows.nth(1).click({ button: 'right' });
  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();
  await expect(menu.getByRole('button', { name: 'Edit node…', exact: true })).toHaveCount(0);
  await expect(
    menu.getByRole('button', { name: `Edit "${name}"…`, exact: true }),
  ).toBeVisible();
  await expect(menu.getByRole('button', { name: 'Pin', exact: true })).toHaveCount(0);
  await expect(menu.getByRole('button', { name: `Pin "${name}"`, exact: true })).toBeVisible();
});

test('a row-only action is unqualified when nothing else is selected', async ({ page }) => {
  // The other half: naming the row on every menu would be noise, and would stop the name from
  // meaning "careful, this one is not the batch".
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  await rows.nth(1).click({ button: 'right' });
  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();
  await expect(menu.getByRole('button', { name: 'Edit node…', exact: true })).toBeVisible();
  await expect(menu.getByRole('button', { name: 'Pin', exact: true })).toBeVisible();
});

test('the selection bar offers tagging, the verb that was right-click only', async ({ page }) => {
  // 🚨 ADR-124 増分 9. `POST /nodes/tags` and `BulkTagModal` both shipped in 増分 6's wake, reachable
  // only by right-clicking a checked row — so an operator working from the bar could not learn that
  // bulk tagging exists. The bar and the menu must offer the same verbs.
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  await rows.nth(0).click();
  await rows.nth(1).click({ modifiers: ['ControlOrMeta'] });

  const bar = page.locator('.nodes-selbar');
  await expect(bar).toBeVisible();
  const tag = bar.getByRole('button', { name: 'Tag…', exact: true });
  await expect(tag).toBeVisible();
  await tag.click();
  // The dialog acts on the whole working set, not on whichever row was clicked last.
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText('2');
});

test('the pool chips act on the whole selection, not the row that was clicked', async ({ page }) => {
  // 🚨 ADR-124 増分 10. The chips sat in a menu headed "Move 3 selected…" and wrote exactly one
  // node — the same defect Inc.2 fixed for the moves, left in the section that had no bulk form.
  const seen: { node_ids: string[]; pool?: string }[] = [];
  await page.route('**/api/v1/nodes/pool', async (route) => {
    const body = route.request().postDataJSON() as { node_ids: string[]; pool?: string };
    seen.push(body);
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ requested: body.node_ids.length, applied: body.node_ids.length }),
    });
  });
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  await rows.nth(0).click();
  await rows.nth(2).click({ modifiers: ['Shift'] });
  await expect(page.locator('.ntree-row.checked')).toHaveCount(3);

  await rows.nth(1).click({ button: 'right' });
  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();
  // The heading says which scope the chips have, so the label and the write cannot disagree.
  await expect(menu).toContainText('Poller pool — 3 selected');
  // "Inherit" is a chip like any other and is always present, so it is the one to press without
  // depending on which pools the mock happens to offer.
  await menu.getByRole('button', { name: 'Inherit', exact: true }).click();

  await expect.poll(() => seen.length).toBe(1);
  expect(seen[0].node_ids, 'the chip wrote fewer nodes than were marked').toHaveLength(3);
  expect(seen[0].pool).toBe('');
  await expect(page.locator('.ntree-row.checked')).toHaveCount(0);
});

test('the pool chips still act on one row when nothing else is selected', async ({ page }) => {
  // The other half: a lone row keeps the single-node write, so the batch endpoint does not become
  // the only way to change one node's pool.
  const bulk: unknown[] = [];
  const single: unknown[] = [];
  await page.route('**/api/v1/nodes/pool', async (route) => {
    bulk.push(route.request().postDataJSON());
    await route.fulfill({ status: 200, contentType: 'application/json', body: '{}' });
  });
  await page.route('**/api/v1/nodes/*/pool', async (route) => {
    single.push(route.request().postDataJSON());
    await route.fulfill({ status: 204, body: '' });
  });
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  await rows.nth(1).click({ button: 'right' });
  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();
  await expect(menu).toContainText('Poller pool');
  await expect(menu).not.toContainText('selected');
  await menu.getByRole('button', { name: 'Inherit', exact: true }).click();

  await expect.poll(() => single.length).toBe(1);
  expect(bulk, 'a lone row went through the batch endpoint').toHaveLength(0);
});

test('the selection bar can set the pool on every selected node', async ({ page }) => {
  const seen: { node_ids: string[]; pool?: string }[] = [];
  await page.route('**/api/v1/nodes/pool', async (route) => {
    const body = route.request().postDataJSON() as { node_ids: string[]; pool?: string };
    seen.push(body);
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ requested: body.node_ids.length, applied: body.node_ids.length }),
    });
  });
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  await rows.nth(0).click();
  await rows.nth(1).click({ modifiers: ['ControlOrMeta'] });

  const bar = page.locator('.nodes-selbar');
  await bar.getByRole('button', { name: 'More…', exact: true }).click();
  await page.getByRole('menu').getByRole('menuitem', { name: 'Poller pool…' }).click();

  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.form-targets li')).toHaveCount(2);
  await dialog.getByRole('textbox').first().fill('osaka');
  await dialog.getByRole('button', { name: 'Save', exact: true }).click();

  await expect.poll(() => seen.length).toBe(1);
  expect(seen[0].node_ids).toHaveLength(2);
  expect(seen[0].pool).toBe('osaka');
});

test('a maintenance preset covers the whole selection, not the row that was clicked', async ({
  page,
}) => {
  // 🚨 ADR-124 増分 11, and the worst instance of the Inc.2 defect: an operator silencing a dozen
  // devices for tonight's work covered exactly one, and found out by being paged during it.
  const seen: { node_ids: string[] }[] = [];
  await page.route('**/api/v1/maintenance-windows/bulk', async (route) => {
    const body = route.request().postDataJSON() as { node_ids: string[] };
    seen.push(body);
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ requested: body.node_ids.length, created: body.node_ids.length }),
    });
  });
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  await rows.nth(0).click();
  await rows.nth(2).click({ modifiers: ['Shift'] });
  await expect(page.locator('.ntree-row.checked')).toHaveCount(3);

  await rows.nth(1).click({ button: 'right' });
  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();
  await expect(menu).toContainText('Maintenance — 3 selected');
  await expect(menu).toContainText('Mute — 3 selected');
  await menu.getByRole('button', { name: '4h', exact: true }).first().click();

  await expect.poll(() => seen.length).toBe(1);
  expect(seen[0].node_ids, 'the preset suppressed fewer nodes than were marked').toHaveLength(3);
  await expect(page.locator('.ntree-row.checked')).toHaveCount(0);
});

test('a mute preset on a lone row still writes the single-node mute', async ({ page }) => {
  // The other half: one row keeps the single write, so the batch route does not become the only
  // way to mute one node.
  const bulk: unknown[] = [];
  const single: unknown[] = [];
  await page.route('**/api/v1/mutes/bulk', async (route) => {
    bulk.push(route.request().postDataJSON());
    await route.fulfill({ status: 200, contentType: 'application/json', body: '{}' });
  });
  await page.route('**/api/v1/mutes', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    single.push(route.request().postDataJSON());
    await route.fulfill({
      status: 201,
      contentType: 'application/json',
      body: JSON.stringify({ id: '00000000-0000-4000-8000-00000000000f' }),
    });
  });
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  await rows.nth(1).click({ button: 'right' });
  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();
  await expect(menu).toContainText('Mute');
  await expect(menu).not.toContainText('selected');
  // Two sections carry a "1h" chip; the mute one is the second.
  await menu.getByRole('button', { name: '1h', exact: true }).nth(1).click();

  await expect.poll(() => single.length).toBe(1);
  expect(bulk, 'a lone row went through the batch endpoint').toHaveLength(0);
});

test('the selection bar can open a maintenance window over every selected node', async ({
  page,
}) => {
  const seen: { node_ids: string[]; name: string }[] = [];
  await page.route('**/api/v1/maintenance-windows/bulk', async (route) => {
    const body = route.request().postDataJSON() as { node_ids: string[]; name: string };
    seen.push(body);
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ requested: body.node_ids.length, created: body.node_ids.length }),
    });
  });
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  await rows.nth(0).click();
  await rows.nth(1).click({ modifiers: ['ControlOrMeta'] });

  await page
    .locator('.nodes-selbar')
    .getByRole('button', { name: 'More…', exact: true })
    .click();
  await page.getByRole('menu').getByRole('menuitem', { name: 'Maintenance…' }).click();

  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  // The scope controls are locked and the nodes are listed, so what is about to be written is on
  // screen before the operator presses the button.
  await expect(dialog.locator('.form-targets li')).toHaveCount(2);
  const boxes = dialog.locator('input[type="datetime-local"]');
  await boxes.nth(0).fill('2030-01-01T00:00');
  await boxes.nth(1).fill('2030-01-01T02:00');
  await dialog.getByRole('button', { name: 'Add window', exact: true }).click();

  await expect.poll(() => seen.length).toBe(1);
  expect(seen[0].node_ids).toHaveLength(2);
});
