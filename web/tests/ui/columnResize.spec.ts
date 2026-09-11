// SPDX-License-Identifier: AGPL-3.0-only
// Column resize (ADR-129) — dragging a grip, typing at one, the two resets, and the rule that
// matters more than any of them.
//
// Why Tier1 and not a unit test: `lib/columnWidths.test.ts` owns the arithmetic, and none of it can
// say whether the grip is in the right grid track, whether the column actually took the width, or
// whether **the three grids still agree afterwards**. That last one is the point of this file.
// `.dt-head`, `.dt-filters` and every `.dt-row` resolve from one template string (ADR-054); a
// resize is precisely the edit that can desynchronise them, and the symptom is silent — filter
// controls slide out from under their headings and nothing throws.
//
// ⚠️ No assertion here names a pixel the CSS chose. `1fr` and the window size decide most tracks,
// so the tests assert *relations*: it grew, the others did not move, the three grids match.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';

/** The Events log — a `DataTable` with a filter row, so all three grids are on screen at once. */
const TABLE = '.dt';
const HEAD = '.dt-head';

/** The resolved track list of one grid, as the browser laid it out. */
async function tracks(page: import('@playwright/test').Page, selector: string) {
  return page
    .locator(selector)
    .first()
    .evaluate((el) => getComputedStyle(el).gridTemplateColumns);
}

/** The stored widths for one table, read straight out of the persisted store. */
async function stored(page: import('@playwright/test').Page, tableId: string) {
  return page.evaluate((id) => {
    const doc = (
      JSON.parse(localStorage.getItem('yagra_prefs') ?? '{}') as {
        state?: { tableColumnWidths?: Record<string, Record<string, number>> };
      }
    ).state?.tableColumnWidths;
    return doc?.[id] ?? null;
  }, tableId);
}

test('every column carries a grip, announced as a control with real bounds', async ({ page }) => {
  await page.goto('/events');
  await expect(page.locator(TABLE).first()).toBeVisible();

  const grips = page.locator(`${HEAD} .colresize`);
  const headings = page.locator(`${HEAD} > .dt-h`);
  const n = await headings.count();
  expect(n, 'no header cells were inspected — the selector stopped matching').toBeGreaterThan(3);
  await expect(grips).toHaveCount(n);

  const first = grips.first();
  await expect(first).toHaveAttribute('role', 'slider');
  await expect(first).toHaveAttribute('aria-orientation', 'horizontal');
  // Explicit bounds, not inherited defaults — the Geo map handle omits them and this family is not
  // supposed to copy it on that point (`ui-conventions.md`).
  await expect(first).toHaveAttribute('aria-valuemin', /\d+/);
  await expect(first).toHaveAttribute('aria-valuemax', /\d+/);
});

test('the headings share the grips’ row instead of being pushed below them', async ({ page }) => {
  // 🚨 THE FAILURE THIS EXISTS FOR, and the first version of it could not see it. The grips are
  // placed at an explicit row AND column; a header cell given only `grid-column` is still
  // auto-placed down the rows, so it lands in row 2 and the whole header renders one band lower.
  // That shipped (ADR-129) — and the check written for it compared the headings **to each other**,
  // which is exactly the quantity that does not change when they all move together.
  //
  // So compare each heading to the thing that displaced it: its own grip. They occupy one grid
  // cell, so their boxes must overlap vertically. And assert the header did not grow a second row.
  await page.goto('/events');
  await expect(page.locator(TABLE).first()).toBeVisible();

  const overlap = await page.locator(HEAD).first().evaluate((head) => {
    const rowH = head.getBoundingClientRect().height;
    const cells = [...head.querySelectorAll(':scope > .dt-h')];
    const grips = [...head.querySelectorAll(':scope > .colresize')];
    return {
      rowH,
      pairs: cells.length,
      apart: cells.map((c, i) => {
        const a = c.getBoundingClientRect();
        const b = grips[i]?.getBoundingClientRect();
        return b ? Math.max(a.top - b.bottom, b.top - a.bottom) : Number.NaN;
      }),
    };
  });

  expect(overlap.pairs, 'no header cells were inspected').toBeGreaterThan(3);
  for (const [i, gap] of overlap.apart.entries()) {
    expect(gap, `heading ${i} does not share a row with its own grip`).toBeLessThan(0);
  }
  // `.dt-head` declares `height: 38px`, so a second implicit row cannot make it taller — it clips
  // instead, which is why the overlap check above is the one that has to hold. The Interfaces list
  // uses `min-height` and does grow, so both symptoms are covered between here and its own test.
  expect(overlap.rowH, 'the header row grew a second band').toBeLessThan(48);
});

test('dragging a grip widens its own column and leaves the others where they were', async ({
  page,
}) => {
  await page.goto('/events');
  await expect(page.locator(TABLE).first()).toBeVisible();

  const before = (await tracks(page, HEAD)).split(' ').map(parseFloat);
  const grip = page.locator(`${HEAD} .colresize`).first();
  const box = (await grip.boundingBox())!;

  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  // Two moves rather than one: the drag is computed from the gesture origin, so a single jump
  // would pass even if the handler accumulated deltas per event.
  await page.mouse.move(box.x + box.width / 2 + 60, box.y + box.height / 2);
  await page.mouse.move(box.x + box.width / 2 + 120, box.y + box.height / 2);
  await page.mouse.up();

  const after = (await tracks(page, HEAD)).split(' ').map(parseFloat);
  expect(after[0], 'the dragged column did not widen').toBeGreaterThan(before[0] + 80);
  // 🚨 The assertion this test exists for. The table grows rather than stealing from the neighbour
  // (ADR-129 decision 2): the point of the feature is to reveal truncated text, not to move the
  // truncation one column along. It only holds because the gesture freezes every column at the
  // width it already had — measured before that landed, dragging the first column +80px took
  // exactly 80px off the other flexible track and the table never widened at all.
  for (let i = 1; i < before.length; i += 1) {
    expect(Math.abs(after[i] - before[i]), `column ${i} moved when its neighbour was dragged`)
      .toBeLessThan(3);
  }
  const sum = (a: number[]) => a.reduce((x, y) => x + y, 0);
  expect(sum(after) - sum(before), 'the table did not take the space from the pane').toBeGreaterThan(
    80,
  );
});

test('the three grids still resolve to the same tracks after a drag', async ({ page }) => {
  // 🚨 This is the one that must never go red quietly. `DataTable` builds ONE template string and
  // hands it to the header, the filter row and every data row; ADR-054 exists because they once
  // resolved it to different widths and the last columns were drawn past an unreachable edge.
  await page.goto('/events');
  await expect(page.locator(TABLE).first()).toBeVisible();
  await expect(page.locator('.dt-filters').first()).toBeVisible();

  const grip = page.locator(`${HEAD} .colresize`).first();
  const box = (await grip.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 90, box.y + box.height / 2);
  await page.mouse.up();

  const [head, filters, row] = await Promise.all([
    tracks(page, HEAD),
    tracks(page, '.dt-filters'),
    tracks(page, '.dt-row'),
  ]);
  expect(head.split(' ').length).toBeGreaterThan(3);
  expect(filters, 'the filter row drifted from the header after a resize').toBe(head);
  expect(row, 'the data rows drifted from the header after a resize').toBe(head);
});

test('one gesture is one write, and a double-click releases the column it is on', async ({
  page,
}) => {
  // ⚠️ This deliberately does NOT reload to prove persistence. `tests/support/app.ts` seeds
  // `yagra_prefs` from an `addInitScript` that runs on *every* navigation and overwrites the whole
  // object, so a reload here would always report the default and the test would be about the
  // harness. What is left — and is the half that can actually break — is that a gesture writes a
  // value: the store is `persist`ed, so a written value is a remembered value.
  await page.goto('/events');
  await expect(page.locator(TABLE).first()).toBeVisible();
  expect(await stored(page, 'events.log'), 'a width existed before anything was dragged').toBeNull();

  const original = (await tracks(page, HEAD)).split(' ').map(parseFloat);
  const grip = page.locator(`${HEAD} .colresize`).first();
  const box = (await grip.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 100, box.y + box.height / 2);
  await page.mouse.up();

  const saved = await stored(page, 'events.log');
  expect(saved, 'the drag stored nothing').not.toBeNull();
  // One gesture stores every column, not only the one under the pointer: the drag freezes the
  // table at the widths it already had so the space it takes comes from the pane. What must be
  // true is that it is ONE write — the store holds exactly the columns the header draws, and each
  // one matches what the browser actually laid out.
  const drawn = (await tracks(page, HEAD)).split(' ').map(parseFloat);
  expect(Object.keys(saved!).length, 'the drag did not freeze the table').toBe(drawn.length);
  for (const px of Object.values(saved!)) {
    expect(drawn.some((d) => Math.abs(d - px) < 3), `stored ${px}px, which no column was drawn at`)
      .toBe(true);
  }

  // A double-click resets **one** column, not the table: the rest keep the widths the gesture
  // froze them at, and the reset control in the header is the full undo. What has to be true is
  // that exactly one entry goes and that the column goes back to the width it was drawn at before
  // anything was dragged.
  await grip.dblclick();
  const afterReset = (await stored(page, 'events.log')) ?? {};
  expect(Object.keys(afterReset).length, 'double-click released more than its own column').toBe(
    Object.keys(saved!).length - 1,
  );
  expect(
    Math.abs((await tracks(page, HEAD)).split(' ').map(parseFloat)[0] - original[0]),
    'double-click did not give the column its original width back',
  ).toBeLessThan(3);
});

test('a grip is operable from the keyboard', async ({ page }) => {
  await page.goto('/events');
  await expect(page.locator(TABLE).first()).toBeVisible();

  const before = (await tracks(page, HEAD)).split(' ').map(parseFloat)[0];
  await page.locator(`${HEAD} .colresize`).first().focus();
  for (let i = 0; i < 4; i += 1) await page.keyboard.press('ArrowRight');
  const wider = (await tracks(page, HEAD)).split(' ').map(parseFloat)[0];
  expect(wider, 'ArrowRight did not widen the column').toBeGreaterThan(before);

  for (let i = 0; i < 4; i += 1) await page.keyboard.press('ArrowLeft');
  expect(Math.abs((await tracks(page, HEAD)).split(' ').map(parseFloat)[0] - before)).toBeLessThan(3);
});

test('the reset control appears only once something has been dragged', async ({ page }) => {
  // A grip's double-click resets one column and has no on-screen affordance at all. Showing this
  // button only when there is something to undo is what keeps an untouched header exactly as it
  // was while still answering `ui-conventions.md` R6 for anyone who has changed something.
  await page.goto('/events');
  await expect(page.locator(TABLE).first()).toBeVisible();
  await expect(page.locator('.colresize-reset')).toHaveCount(0);

  const grip = page.locator(`${HEAD} .colresize`).first();
  const box = (await grip.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 100, box.y + box.height / 2);
  await page.mouse.up();

  const reset = page.locator('.colresize-reset').first();
  await expect(reset).toBeVisible();
  // Reachable, not merely present: a `opacity: 0` element counts as visible to Playwright, which is
  // how ten screens once shipped with row actions no human could see (`ui-conventions.md`).
  expect(
    await reset.evaluate((el) => Number(getComputedStyle(el).opacity)),
  ).toBeGreaterThan(0.5);

  await reset.click();
  expect(await stored(page, 'events.log'), 'the reset left widths behind').toBeNull();
  await expect(page.locator('.colresize-reset')).toHaveCount(0);
});

test.describe('the node-detail Interfaces list', () => {
  // The other surface this shipped for, and the one that is NOT a `DataTable` — it keeps its own
  // `.nd-if-*` grid, whose template moved out of CSS and into an inline custom property for this.
  // Its own describe because reaching the tab needs a node in the mock, which is file-scoped
  // configuration everywhere else in this suite.
  const NODE_ID = '00000000-0000-4000-8000-0000000000aa';
  test.use({
    viewport: { width: 1600, height: 1000 },
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID),
      },
    },
  });

  test('gets the same grips, driven by the custom property rather than the CSS', async ({
    page,
  }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=interfaces`);
    await expect(page.locator('.nd-if-head')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('.nd-if-row').first()).toBeVisible({ timeout: 15_000 });

    const grips = page.locator('.nd-if-head .colresize');
    await expect(grips).toHaveCount(9);

    // 🚨 The symptom the operator actually reported: `.nd-if-head` is `min-height: 32px`, so a
    // heading pushed into an implicit second row does not clip — the band doubles and the labels
    // sit along the bottom of it. That is what shipped, and it is why this assertion is a height
    // and not a comparison between the headings (which all moved together and stayed level).
    const headH = await page
      .locator('.nd-if-head')
      .evaluate((el) => el.getBoundingClientRect().height);
    expect(headH, 'the Interfaces header grew a second row under the grips').toBeLessThan(40);

    const before = (await tracks(page, '.nd-if-head')).split(' ').map(parseFloat);
    const box = (await grips.first().boundingBox())!;
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 + 50, box.y + box.height / 2);
    await page.mouse.move(box.x + box.width / 2 + 100, box.y + box.height / 2);
    await page.mouse.up();

    const after = (await tracks(page, '.nd-if-head')).split(' ').map(parseFloat);
    // 🚨 If the custom property never reached the grid, the CSS fallback still draws nine perfectly
    // good tracks and every other assertion in this file would pass. A drag moving one is the only
    // thing that distinguishes "the property is applied" from "the fallback is".
    expect(after[0], 'the custom property never reached the grid').toBeGreaterThan(before[0] + 60);

    // Same rule as the DataTable case: one template, three grids (ADR-054).
    const [head, filters, row] = await Promise.all([
      tracks(page, '.nd-if-head'),
      tracks(page, '.nd-if-filters'),
      tracks(page, '.nd-if-row'),
    ]);
    expect(filters, 'the filter row drifted from the header after a resize').toBe(head);
    expect(row, 'the data rows drifted from the header after a resize').toBe(head);
  });
});

test('a phone gets no grips at all', async ({ page }) => {
  // There are no columns in card mode — `DataTable` does not render `.dt-head` there, and
  // `.nd-if-head` is `display: none`. A grip that survived would be a focusable control with
  // nothing behind it.
  //
  // ⚠️ The viewport alone is not enough: `tests/support/app.ts` seeds `uiMode: 'desktop'`, which
  // pins the desktop shell however narrow the window is. This overwrites that seed — the fixture's
  // init script is registered first, so a later one wins — and `data-viewport` is asserted rather
  // than assumed, because a silent failure here would make the test pass for the wrong reason.
  await page.setViewportSize({ width: 390, height: 780 });
  await page.addInitScript(() => {
    localStorage.setItem(
      'yagra_prefs',
      JSON.stringify({
        state: { theme: 'dark', language: 'en', uiMode: 'auto', filterRowOpen: true },
        version: 0,
      }),
    );
  });
  await page.goto('/events');
  await expect(page.locator('html')).toHaveAttribute('data-viewport', 'mobile');
  await expect(page.locator('.dt-card').first()).toBeVisible();
  await expect(page.locator('.colresize')).toHaveCount(0);
  await expect(page.locator('.colresize-reset')).toHaveCount(0);
});
