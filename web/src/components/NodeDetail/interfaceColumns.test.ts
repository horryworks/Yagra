// SPDX-License-Identifier: AGPL-3.0-only
// `interfaceColumns.ts` against the fallback still written in `NodeDetail.css` (ADR-129).
//
// This is a mirror, and the repo's rule is that a mirror gets a test in the same commit or it is
// not allowed to exist (`extensibility.md` §2). The mirror is deliberate: the resolved template is
// handed to the grids as an inline **custom property** — never as an inline `grid-template-columns`,
// which would beat the mobile media rules — and a custom property needs a fallback, because the
// alternative when it is absent is a one-column grid. So the CSS keeps its own copy of the tracks
// and this test is what stops the two drifting.
//
// 🚨 What drift looks like if nothing checks: someone widens a column in the CSS to fix a
// truncation, every operator who has never dragged anything sees the fix, and every operator who
// has dragged *some other* column keeps the old width for this one — because the moment one width
// is stored the whole template comes from TypeScript.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { TFunction } from 'i18next';
import { filterSlots, INTERFACE_COLUMNS } from './interfaceColumns';
import { interfaceColumns } from './tabFilters';

const CSS = readFileSync(join(__dirname, 'NodeDetail.css'), 'utf8');

/** The keys the filter row has a control for — read from the real specs, not restated. */
const FILTER_KEYS = interfaceColumns(((k: string) => k) as unknown as TFunction).map((c) => c.key);

/** The fallback track list out of `grid-template-columns: var(--nd-if-cols, …)`. */
function cssFallback(): string {
  const m = /grid-template-columns:\s*var\(\s*--nd-if-cols\s*,([\s\S]*?)\)\s*;/.exec(CSS);
  if (!m) throw new Error('the --nd-if-cols declaration is gone from NodeDetail.css');
  return m[1].trim().replace(/\s+/g, ' ');
}

describe('the Interfaces column list', () => {
  it('has the ten columns the header draws', () => {
    // The accepting case first: a list that had lost a column would satisfy the comparison below
    // just as well, because the CSS would be edited to match it.
    expect(INTERFACE_COLUMNS).toHaveLength(10);
    expect(INTERFACE_COLUMNS.map((c) => c.key)).toEqual([
      'if_name',
      'if_alias',
      'neighbors',
      'oper',
      'media',
      'speed',
      'duplex',
      'throughput',
      'in',
      'out',
    ]);
  });

  it('names every column exactly once', () => {
    expect(new Set(INTERFACE_COLUMNS.map((c) => c.key)).size).toBe(INTERFACE_COLUMNS.length);
  });

  it('declares the same tracks the CSS falls back to', () => {
    expect(cssFallback()).toBe(INTERFACE_COLUMNS.map((c) => c.width).join(' '));
  });

  it('declares no `auto` track', () => {
    // An `auto` track sizes to its own content, so the header, the filter row and the data rows
    // would resolve one template to three different widths (ADR-054, and `NodeDetail.css` carries
    // the note about the time a <button>'s UA padding did exactly that here).
    for (const c of INTERFACE_COLUMNS) expect(c.width).not.toMatch(/\bauto\b/);
  });
});

describe('filterSlots', () => {
  it('puts each filter control under its own heading, one slot per column', () => {
    // Pinned by position, because a slot one place off draws a control under the wrong heading and
    // nothing else notices — NEIGHBORS (ADR-145) is the column that carries none, third.
    expect(filterSlots(FILTER_KEYS)).toEqual([
      'if_name',
      'if_alias',
      null,
      'oper',
      'media',
      'speed',
      'duplex',
      null,
      null,
      null,
    ]);
  });

  it('places every filter spec, so a renamed key cannot silently lose its control', () => {
    expect(FILTER_KEYS.length).toBeGreaterThan(0);
    expect(filterSlots(FILTER_KEYS).filter((s) => s !== null)).toEqual(FILTER_KEYS);
  });
});
