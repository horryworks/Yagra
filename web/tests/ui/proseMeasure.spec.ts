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

test('a step inside a closed step frame is not counted, and is once the frame is open', async ({
  page,
}) => {
  // Chromium keeps a box for the content of a closed <details>, so without the exclusion every
  // step frame (ADR-200 kind d) would count as prose on the screen that hides it.
  await page.goto('/settings/roles');
  await expect(page.locator('.pageheader-note')).toHaveCount(1);
  const before = (await inspectScreenGeometry(page)).prose.chars;

  await page.evaluate((text) => {
    const details = document.createElement('details');
    details.id = 'prose-measure-frame';
    const summary = document.createElement('summary');
    summary.textContent = 'Steps';
    const li = document.createElement('li');
    li.textContent = text;
    const ol = document.createElement('ol');
    ol.appendChild(li);
    details.append(summary, ol);
    document.querySelector('.shell-content')?.appendChild(details);
  }, SENTENCE);
  expect((await inspectScreenGeometry(page)).prose.chars).toBe(before);

  await page.evaluate(() => {
    const d = document.getElementById('prose-measure-frame') as HTMLDetailsElement | null;
    if (d) d.open = true;
  });
  expect((await inspectScreenGeometry(page)).prose.chars).toBe(before + 60);
});
