// SPDX-License-Identifier: AGPL-3.0-only
// The walk's tenth check (ADR-200), proven both ways on one screen: prose the screen writes is
// counted, and prose in the page note is not. A counter that only ever sees screens pass cannot
// tell "no prose" from "looked at nothing".

import { expect, test } from '../support/app';
import { inspectScreenGeometry } from './screenGeometry';

const SENTENCE = 'This sentence is sixty characters long, exactly, for a test.';

test('a paragraph the screen adds is counted, and one in the page note is not', async ({ page }) => {
  expect(SENTENCE.length).toBe(60);
  await page.goto('/settings/roles');
  await expect(page.locator('.pageheader-note')).toHaveCount(1);
  const before = (await inspectScreenGeometry(page)).prose.chars;

  await page.evaluate((text) => {
    const p = document.createElement('p');
    p.textContent = text;
    document.querySelector('.shell-content')?.appendChild(p);
  }, SENTENCE);
  const added = (await inspectScreenGeometry(page)).prose;
  expect(added.chars).toBe(before + 60);
  expect(added.samples.some((s) => SENTENCE.startsWith(s))).toBe(true);

  await page.evaluate((text) => {
    const span = document.createElement('span');
    span.textContent = text;
    document.querySelector('.pageheader-note')?.appendChild(span);
  }, SENTENCE);
  expect((await inspectScreenGeometry(page)).prose.chars).toBe(before + 60);
});
