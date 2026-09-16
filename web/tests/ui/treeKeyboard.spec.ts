// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree from the keyboard (ADR-155).
//
// Why a browser and not only `nodeTreeKeys.test.ts`: that file decides which row a key lands on, and
// nothing there can say whether the key reaches the tree at all, whether focus is where the next key
// needs it, whether the row the key landed on is scrolled into view, or how many times the URL was
// written while a key was held. Those are wiring, layout and timing.
//
// ⚠️ **Every URL assertion polls.** A key moves `.sel` at once and writes `?sel=` only once the keys
// have rested (`CURSOR_SETTLE_MS`), so reading the URL straight after a press reads the previous row.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, TREE_SIBLING_IDS } from '../support/bootstrap';
import {
  CHILD_ID,
  CHILD_NAME,
  groupSummary,
  membersByGroup,
  NODE_ID,
  NODE_NAME,
  PARENT_ID,
  PARENT_NAME,
  twoLevelGroups,
} from '../support/twoLevelTree';
import { RUN_LENGTH, runNodeId, runNodeName, ungroupedRunByGroup } from '../support/ungroupedRun';

type Page = import('@playwright/test').Page;

/** The selection lives in `?sel=`. */
const selected = (page: Page) => new URL(page.url()).searchParams.get('sel');

const body = (page: Page) => page.locator('.ntree-body');
/** A row by the id it carries for `aria-activedescendant` — the attribute form, because the id holds
 *  a colon and a `#` selector would read it as a pseudo-class. */
const rowById = (page: Page, kind: 'n' | 'g', id: string) => page.locator(`[id="ntree-${kind}:${id}"]`);

/** Whether keyboard focus is inside `selector` (or is it). */
const focusIn = (page: Page, selector: string) =>
  page.evaluate((s) => !!document.activeElement?.closest(s), selector);

test.describe('a folder inside a folder', () => {
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/node-groups': twoLevelGroups(),
        '/api/v1/nodes/by-group': membersByGroup,
        '/api/v1/fleet/group-summary': groupSummary(),
      },
    },
  });

  const parentRow = (page: Page) => page.locator('.ntree-grow').filter({ hasText: PARENT_NAME });
  const childRow = (page: Page) => page.locator('.ntree-grow').filter({ hasText: CHILD_NAME });
  const member = (page: Page) => page.locator('.ntree-body').getByText(NODE_NAME, { exact: true });

  test('Up and Down move the selection, and the URL follows once the key rests', async ({ page }) => {
    await page.goto('/nodes');
    await expect(member(page)).toBeVisible();

    await parentRow(page).click();
    await expect.poll(() => selected(page)).toBe(`group:${PARENT_ID}`);

    await page.keyboard.press('ArrowDown');
    // The row moves at once; the URL follows when the key rests.
    await expect(page.locator('.ntree-row.sel')).toHaveCount(1);
    await expect(childRow(page)).toHaveClass(/\bsel\b/);
    await expect.poll(() => selected(page)).toBe(`group:${CHILD_ID}`);

    await page.keyboard.press('ArrowDown');
    await expect.poll(() => selected(page)).toBe(`node:${NODE_ID}`);
    await expect(body(page)).toHaveAttribute('aria-activedescendant', `ntree-n:${NODE_ID}`);
    // After a click focus was on the row's name button; a claimed key pulls it onto the tree itself.
    expect(await page.evaluate(() => document.activeElement?.classList.contains('ntree-body'))).toBe(
      true,
    );

    await page.keyboard.press('ArrowUp');
    await expect.poll(() => selected(page)).toBe(`group:${CHILD_ID}`);
    await expect(page.locator('.ntree-row.sel')).toHaveCount(1);
  });

  test('Left and Right close, open, and step between a folder and its contents', async ({ page }) => {
    await page.goto('/nodes');
    await expect(member(page)).toBeVisible();
    await member(page).click();
    await expect.poll(() => selected(page)).toBe(`node:${NODE_ID}`);

    // From a node, Left goes to its folder.
    await page.keyboard.press('ArrowLeft');
    await expect.poll(() => selected(page)).toBe(`group:${CHILD_ID}`);
    await expect(childRow(page)).toHaveAttribute('aria-expanded', 'true');

    // On an open folder, Left closes it — through the ▶'s own path.
    await page.keyboard.press('ArrowLeft');
    await expect(member(page)).toHaveCount(0);
    await expect(childRow(page)).toHaveAttribute('aria-expanded', 'false');
    await expect(childRow(page).locator('.ntree-twisty')).not.toHaveClass(/\bopen\b/);

    // On a closed folder, Left goes up.
    await page.keyboard.press('ArrowLeft');
    await expect.poll(() => selected(page)).toBe(`group:${PARENT_ID}`);

    // On an open folder, Right steps in; on a closed one it opens; on an open one again it steps in.
    await page.keyboard.press('ArrowRight');
    await expect.poll(() => selected(page)).toBe(`group:${CHILD_ID}`);
    await page.keyboard.press('ArrowRight');
    await expect(member(page)).toBeVisible();
    await page.keyboard.press('ArrowRight');
    await expect.poll(() => selected(page)).toBe(`node:${NODE_ID}`);

    // Enter on a node opens its own page.
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(new RegExp(`/nodes/${NODE_ID}$`));
  });

  test('the tree is one Tab stop, and Tab selects nothing', async ({ page }) => {
    await page.goto('/nodes');
    await expect(member(page)).toBeVisible();

    await body(page).focus();
    expect(selected(page)).toBeNull();
    await page.keyboard.press('Tab');
    // 🚨 The regression this pins: every ▶ and every name button used to be a Tab stop, so leaving
    // the tree took one press per row.
    expect(await focusIn(page, '.ntree-body'), 'Tab stayed inside the tree').toBe(false);

    await page.keyboard.press('Shift+Tab');
    expect(await focusIn(page, '.ntree-body'), 'Shift+Tab did not come back to the tree').toBe(true);
    expect(selected(page), 'moving focus selected a row').toBeNull();

    // The first Down is what picks a row.
    await page.keyboard.press('ArrowDown');
    await expect.poll(() => selected(page)).toBe(`group:${PARENT_ID}`);
  });

  test('Escape still clears a selection the keyboard made, and it stays cleared', async ({ page }) => {
    await page.goto('/nodes');
    await expect(member(page)).toBeVisible();
    await parentRow(page).click();
    await page.keyboard.press('ArrowDown');
    await expect.poll(() => selected(page)).toBe(`group:${CHILD_ID}`);

    await page.keyboard.press('Escape');
    await expect.poll(() => selected(page)).toBeNull();
    await expect(page.locator('.ntree-row.sel')).toHaveCount(0);
    // A cursor left behind would put the selection back once the keys "rested".
    await page.waitForTimeout(400);
    expect(selected(page), 'the selection came back after Escape').toBeNull();
  });

  test('Shift+F10 opens the row menu with focus in it, and Escape gives focus back', async ({ page }) => {
    await page.goto('/nodes');
    await expect(member(page)).toBeVisible();
    await childRow(page).click();
    await expect.poll(() => selected(page)).toBe(`group:${CHILD_ID}`);

    await page.keyboard.press('Shift+F10');
    const menu = page.getByRole('menu');
    await expect(menu).toBeVisible();
    await expect.poll(() => focusIn(page, '.ntree-menu'), 'the menu opened without focus').toBe(true);

    const first = await page.evaluate(() => document.activeElement?.textContent ?? '');
    await page.keyboard.press('ArrowDown');
    const second = await page.evaluate(() => document.activeElement?.textContent ?? '');
    expect(second, 'Down did not move inside the menu').not.toBe(first);
    expect(await focusIn(page, '.ntree-menu')).toBe(true);

    await page.keyboard.press('Escape');
    await expect(menu).toHaveCount(0);
    expect(selected(page), 'Escape closed the menu AND cleared the selection').toBe(`group:${CHILD_ID}`);
    await expect.poll(() => focusIn(page, '.ntree-body'), 'focus did not come back to the tree').toBe(true);

    // And the tree answers the keyboard again straight away.
    await page.keyboard.press('ArrowDown');
    await expect.poll(() => selected(page)).toBe(`node:${NODE_ID}`);
  });
});

test.describe('three nodes in Ungrouped', () => {
  test.use({ mockConfig: { overrides: BOOTSTRAP_OVERRIDES } });

  test('Down walks over the Ungrouped header, and Space and Shift build a working set', async ({ page }) => {
    await page.goto('/nodes');
    const [first, second, third] = TREE_SIBLING_IDS;
    await expect(rowById(page, 'n', third)).toBeVisible();

    const folder = page.locator('.ntree-grow').first();
    await folder.click();
    await expect.poll(() => selected(page)).toMatch(/^group:/);

    // The folder has nothing in it: Right neither opens it nor moves.
    await page.keyboard.press('ArrowRight');
    await expect(folder).not.toHaveAttribute('aria-expanded', /.*/);

    // The next row down is the Ungrouped header, which is not something to select.
    await page.keyboard.press('ArrowDown');
    await expect.poll(() => selected(page)).toBe(`node:${first}`);

    await page.keyboard.press('Space');
    await expect(page.locator('.ntree-row.checked')).toHaveCount(1);
    await expect(page.locator('.nodes-selbar')).toBeVisible();
    // Space marks; it does not move.
    await expect.poll(() => selected(page)).toBe(`node:${first}`);

    await page.keyboard.press('Shift+ArrowDown');
    await expect(page.locator('.ntree-row.checked')).toHaveCount(2);
    await expect.poll(() => selected(page)).toBe(`node:${second}`);

    // Ctrl moves without touching the set.
    await page.keyboard.press('Control+ArrowDown');
    await expect.poll(() => selected(page)).toBe(`node:${third}`);
    await expect(page.locator('.ntree-row.checked')).toHaveCount(2);

    // A plain move abandons it, as a plain click does.
    await page.keyboard.press('ArrowUp');
    await expect(page.locator('.ntree-row.checked')).toHaveCount(0);
    await expect.poll(() => selected(page)).toBe(`node:${second}`);
  });
});

test.describe('a tree taller than its pane', () => {
  test.use({
    mockConfig: {
      overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/nodes/by-group': ungroupedRunByGroup },
    },
  });

  const runRow = (page: Page, i: number) => page.locator('.ntree-node').filter({ hasText: runNodeName(i) });
  const scrollTop = (page: Page) => body(page).evaluate((el) => el.scrollTop);

  test('End and Home bring the row they land on into view', async ({ page }) => {
    await page.goto('/nodes');
    await expect(runRow(page, 0)).toBeVisible();
    const room = await body(page).evaluate((el) => el.scrollHeight - el.clientHeight);
    expect(room, 'the tree does not scroll — this fixture cannot see the bug').toBeGreaterThan(300);

    await runRow(page, 0).click();
    await expect.poll(() => selected(page)).toBe(`node:${runNodeId(0)}`);
    expect(await scrollTop(page)).toBe(0);

    await page.keyboard.press('End');
    const last = RUN_LENGTH - 1;
    await expect.poll(() => selected(page)).toBe(`node:${runNodeId(last)}`);
    // 🚨 Without `scrollToIndex` the selection moves to a row nobody can see — and a virtualized row
    // that far down is not even in the document.
    await expect(runRow(page, last)).toBeInViewport();
    expect(await scrollTop(page)).toBeGreaterThan(0);

    // Home is the first row of the tree — the bootstrap mock's empty folder, above the Ungrouped
    // header — not the first node.
    await page.keyboard.press('Home');
    await expect.poll(() => selected(page)).toMatch(/^group:/);
    await expect.poll(() => scrollTop(page)).toBe(0);

    // PageDown moves a screenful: from the folder, well past the first two nodes.
    await page.keyboard.press('PageDown');
    await expect.poll(() => selected(page)).toMatch(/^node:/);
    const landed = selected(page)!;
    expect(landed, 'PageDown moved one row, not a page').not.toBe(`node:${runNodeId(0)}`);
    expect(landed, 'PageDown moved two rows, not a page').not.toBe(`node:${runNodeId(1)}`);
  });

  test('a run of presses writes the URL once, not once per press', async ({ page }) => {
    // 🚨 THE SAFARI LIMIT. `?sel=` is `history.replaceState`, which Safari refuses past 100 calls in
    // 30 seconds; a held arrow repeats about 30 times a second. Counted in Chromium, which has no such
    // limit, because the count is the property — the limit is only why it matters.
    await page.addInitScript(() => {
      const w = window as unknown as { __replaces: number };
      w.__replaces = 0;
      const original = history.replaceState.bind(history);
      history.replaceState = (...args: Parameters<History['replaceState']>) => {
        w.__replaces += 1;
        return original(...args);
      };
    });
    await page.goto('/nodes');
    await expect(runRow(page, 0)).toBeVisible();
    await runRow(page, 0).click();
    await expect.poll(() => selected(page)).toBe(`node:${runNodeId(0)}`);

    const count = () => page.evaluate(() => (window as unknown as { __replaces: number }).__replaces);
    const before = await count();
    // 🚨 **Six presses paced inside the page, not six `keyboard.press` calls.** Each Playwright press
    // is a round trip to the browser, and under the full suite's eight workers the gap between two of
    // them passed 100 ms — the settle window — so the run wrote the URL four times and failed for a
    // reason that is the test's, not the tree's (measured 2026-09-17). A held key repeats every ~33 ms
    // from inside the browser; this presses every 16 ms, each through the tree's real handler.
    // ⚠️ **Not zero, and not a microtask.** The gap has to be a real timer turn: packed into one task,
    // a settle of 0 ms could not fire between presses either, and this test passed with the settle
    // removed — measured, which is why it is paced.
    await body(page).evaluate(async (el) => {
      for (let i = 0; i < 6; i += 1) {
        el.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true }));
        await new Promise((r) => setTimeout(r, 16));
      }
    });
    await expect.poll(() => selected(page)).toBe(`node:${runNodeId(6)}`);
    await page.waitForTimeout(400);
    const writes = (await count()) - before;
    expect(writes, `six presses wrote the URL ${writes} times`).toBeLessThanOrEqual(2);
    expect(writes, 'the selection moved without the URL being written at all').toBeGreaterThanOrEqual(1);
  });
});
