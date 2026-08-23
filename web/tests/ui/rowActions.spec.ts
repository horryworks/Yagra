// SPDX-License-Identifier: AGPL-3.0-only
// The floors under the walk's row-action check (ADR-088).
//
// The check itself runs per screen inside `walk.spec.ts`, and it is silent by design on a screen
// with no row actions — most settings screens are forms. That silence is exactly what makes it
// worth pinning here: if the class names in `rowActions.ts` stopped matching the app, every screen
// would report "not a subject" and the walk would be green while nothing was checked. This repo has
// shipped that failure three times, so the selector gets a floor of its own.
//
// Source reads, not browser work — they cost nothing and answer a question the browser cannot: not
// "is this screen right" but "is the check still pointed at anything".

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test } from '@playwright/test';

const SRC = join(process.cwd(), 'src');

function filesUnder(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) filesUnder(path, out);
    else if (/\.(tsx?|css)$/.test(name)) out.push(path);
  }
  return out;
}

/** Measured 2026-08-23: fourteen files under `src/` render or style a row-action group — twelve
 *  list screens, the collection editor, and `OverflowMenu`, which drops into the same wrapper. 12
 *  leaves room for a screen to be retired without re-tuning the floor, and is far enough above zero
 *  that a rename cannot pass. */
const MIN_FILES = 12;

test('the row-action classes the walk hovers are still the ones the app renders', () => {
  const hits = filesUnder(SRC).filter((f) => {
    const s = readFileSync(f, 'utf8');
    return s.includes('ytable-actions') || s.includes('il-actions');
  });
  expect(
    hits.length,
    `only ${hits.length} files under src/ mention a row-action group — the selector in rowActions.ts is probably stale, and a stale selector makes the walk's check 9 silently vacuous`,
  ).toBeGreaterThanOrEqual(MIN_FILES);
});

test('every row class that can hold row actions is named in the reveal rules', () => {
  // 🚨 This is the defect itself, written down. `.ytable-actions` is `opacity: 0` until a reveal
  // rule fires, and the rules named `.ytable-row` only — so on every `DataTable` screen (`.dt-row`)
  // the icons were invisible. Reading the stylesheet is the only way to ask the question without
  // opening ten screens, and the browser check next door is what proves the answer is true.
  // ⚠️ **Match the whole selector, never the row class alone.** The first version of this test
  // asked whether `.dt-row:hover` appeared anywhere after the `.ytable-actions` declaration — and
  // when the reveal rule was deleted to check the test worked, the test still passed: the string
  // also occurs in `.dt-row:hover .yt-copy-btn`, a different reveal, forty lines further down. Ten
  // screens failed in the browser and this one stayed green, which is the "quietly green" shape
  // this whole ADR exists to remove. A source check that names half a selector matches half the
  // file.
  const css = readFileSync(join(SRC, 'styles', 'table.css'), 'utf8');
  expect(css.includes('.ytable-actions {'), 'styles/table.css no longer declares it').toBe(true);
  const rules = [
    // hover: the pointer path, and the half that shipped broken.
    '.ytable-row:hover .ytable-actions',
    '.dt-row:hover .ytable-actions',
    '.il-row:hover .il-actions',
    // focus-within: the keyboard path, which the same omission also broke.
    '.ytable-row:focus-within .ytable-actions',
    '.dt-row:focus-within .ytable-actions',
    '.il-row:focus-within .il-actions',
  ];
  for (const rule of rules) {
    expect(
      css.includes(rule),
      `\`${rule}\` is not in styles/table.css — controls on those rows stay transparent`,
    ).toBe(true);
  }
});
