// SPDX-License-Identifier: AGPL-3.0-only
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { isImeComposing } from './ime';

describe('isImeComposing', () => {
  it('is true for the confirming Enter as Chromium and Firefox report it', () => {
    expect(isImeComposing({ nativeEvent: { isComposing: true, keyCode: 229 } })).toBe(true);
    expect(isImeComposing({ isComposing: true })).toBe(true);
  });

  it('is true for Safari, which has already ended the composition and leaves only keyCode 229', () => {
    // The half that a fix reading `isComposing` alone gets wrong.
    expect(isImeComposing({ nativeEvent: { isComposing: false, keyCode: 229 } })).toBe(true);
    expect(isImeComposing({ isComposing: false, keyCode: 229 })).toBe(true);
  });

  it('is false for an ordinary Enter — the guard must not swallow the key it guards', () => {
    expect(isImeComposing({ nativeEvent: { isComposing: false, keyCode: 13 } })).toBe(false);
    expect(isImeComposing({ isComposing: false, keyCode: 13 })).toBe(false);
    expect(isImeComposing({})).toBe(false);
  });
});

// ── Every Enter handler either asks, or says why it need not ─────────────────────────────────────
//
// `grep isComposing web/src` returned nothing for the life of the product, across ten handlers on
// text fields. Nothing else would notice the eleventh: Vitest never runs a `.tsx`, the browser walk
// types in English, and an IME cannot be driven from Playwright.

const SRC = fileURLToPath(new URL('..', import.meta.url));

/** Files that read `'Enter'` and do NOT sit on a text field, each with the reason. A key press on a
 *  button, a row or a list has no composition to respect. */
const NOT_A_TEXT_FIELD: Readonly<Record<string, string>> = {
  'components/NodeDetail/CollectionTab.tsx': 'a metric row acting as a button (role="button")',
  'components/TopologyMap/TopologyMap.tsx': 'an SVG node acting as a button',
  'components/ui/ActionMenu.tsx': 'menu items — focus is on a button, never in a text field',
  'dashboard/primitives/RankedBars.tsx': 'a bar acting as a link',
  'pages/GeoMapPage.tsx': 'a map pin acting as a button',
  'components/NodeTree/nodeTreeKeys.ts':
    'the inventory tree is a listbox; `keyBelongsToTree` already refuses keys typed in its search box',
  // The definition itself, and the decision table that names the key without handling an event.
  'lib/ime.ts': 'the definition',
  'components/shell/searchBox.ts': 'maps a key NAME to an action; its caller (GlobalSearch) asks',
};

function sourceFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return sourceFiles(path);
    return /\.tsx?$/.test(name) && !/\.test\.ts$/.test(name) && !name.endsWith('.d.ts') ? [path] : [];
  });
}

describe('Enter handlers', () => {
  const readers = sourceFiles(SRC)
    .map((path) => ({ rel: relative(SRC, path).split(sep).join('/'), text: readFileSync(path, 'utf8') }))
    // Built at runtime so this file cannot match itself.
    .filter(({ text }) => text.includes(`'${'Enter'}'`));

  it('inspects a real population', () => {
    // A floor on what was INSPECTED: a walk that found nothing would otherwise pass everything.
    expect(readers.length).toBeGreaterThanOrEqual(12);
  });

  it('every file that reads Enter asks the IME, or is listed as not being a text field', () => {
    const silent = readers
      .filter(({ rel }) => !(rel in NOT_A_TEXT_FIELD))
      .filter(({ text }) => !text.includes('isImeComposing('))
      .map(({ rel }) => rel);
    expect(silent, 'reads Enter on what may be a text field without calling isImeComposing').toEqual(
      [],
    );
  });

  it('whoever hands a key NAME to the decision table of the search box asks first', () => {
    // `searchBox.ts` maps a key name to an action and never sees the event, so it cannot ask. Its
    // caller can, and is otherwise invisible to the check above — it never spells the key.
    const callers = sourceFiles(SRC)
      .map((path) => ({ rel: relative(SRC, path).split(sep).join('/'), text: readFileSync(path, 'utf8') }))
      .filter(({ rel, text }) => rel !== 'components/shell/searchBox.ts' && text.includes('keyAction('));
    expect(callers.length).toBeGreaterThanOrEqual(1);
    expect(callers.filter(({ text }) => !text.includes('isImeComposing(')).map((c) => c.rel)).toEqual([]);
  });

  it('the exemption list names only files that still read Enter', () => {
    // An entry whose file moved or stopped handling keys is a reason that excuses nothing.
    const reading = new Set(readers.map((r) => r.rel));
    const stale = Object.keys(NOT_A_TEXT_FIELD).filter((rel) => rel !== 'lib/ime.ts' && !reading.has(rel));
    expect(stale).toEqual([]);
  });
});
