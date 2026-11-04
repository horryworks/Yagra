// SPDX-License-Identifier: AGPL-3.0-only
// Alert grouping on Notification delivery (ADR-236): each level of the switch keeps its name and
// its count on one line. The shared control stretched its buttons to share the width, which broke
// "Certain only · 3" across two lines.
import { expect, test } from '../support/app';

test('each grouping level reads on one line', async ({ page }) => {
  await page.goto('/alerts/routing');
  const group = page.getByRole('group', { name: 'Alert grouping' });
  await expect(group).toBeVisible();
  const buttons = group.getByRole('button');
  await expect(buttons).toHaveCount(4);
  // The mock reports a node down, so every label carries a count.
  // The count says what it counts: the notifications that level would send now.
  await expect(buttons.nth(1)).toContainText('notifies');
  for (const b of await buttons.all()) {
    const box = await b.evaluate((el) => {
      const line = parseFloat(getComputedStyle(el).lineHeight) || 16;
      const range = document.createRange();
      range.selectNodeContents(el);
      return { lines: Math.round(range.getBoundingClientRect().height / line) };
    });
    expect(box.lines, await b.innerText()).toBe(1);
  }
});

test('the wait switch reads on one line and the last seven days are shown', async ({ page }) => {
  await page.goto('/alerts/routing');
  const wait = page.getByRole('group', { name: 'When a neighbour is failing' });
  await expect(wait).toBeVisible();
  const buttons = wait.getByRole('button');
  await expect(buttons).toHaveCount(2);
  await expect(buttons.nth(1)).toHaveText('Wait for the neighbour');
  for (const b of await buttons.all()) {
    const lines = await b.evaluate((el) => {
      const line = parseFloat(getComputedStyle(el).lineHeight) || 16;
      const range = document.createRange();
      range.selectNodeContents(el);
      return Math.round(range.getBoundingClientRect().height / line);
    });
    expect(lines, await b.innerText()).toBe(1);
  }
  // The mock answers a replay, so its line is drawn rather than left out.
  await expect(page.getByText(/^Last 7 days: /)).toBeVisible();
});
