// SPDX-License-Identifier: AGPL-3.0-only
// A doc block that documents nothing, because a declaration was inserted underneath it.
//
// TSDoc attaches a `/** … */` block to the **next** declaration, and only the *last* block wins.
// So when a new function is written between an existing doc and the function it described, the old
// doc silently becomes the new function's, and the function it was written for is left with none:
//
//     /** The org's monitored network ids. */      ← now documents `recordFailures`
//     /** Write which collect tiers are failing. */
//     export function recordFailures() { … }
//     export function monitoredNetworkIds() { … }  ← has no doc at all
//
// Nothing else notices. It compiles, it lints, every test passes, and both sentences are still on
// screen in the editor — they are just above the wrong things. Measured 2026-09-21 during /verify:
// **19 of them**, across widgets, pages, the API client and three Playwright specs, and the same
// increment that introduced one had also *fixed* one by hand (`pages/credentialList.ts`). A class
// this repo recognises and had not mechanized.
//
// 🚨 **The first version of this detector found 4 of the 19.** It treated only a line whose trim is
// `*/` as the end of a block, so a ONE-LINE `/** … */` — which is the shape of most of them — was
// invisible. A checker that under-reports is indistinguishable from a clean tree, which is why the
// recognition cases below are one-liners on purpose and the floor counts files *inspected*.
//
// The Rust half of this class cannot be checked the same way: `/// a` followed by `/// b` is one
// block to rustdoc, so an orphan there is indistinguishable from a two-sentence doc. Two were found
// by hand in the same sweep (`meraki.rs`, `alerts/rules.rs`).
import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';

const SRC = __dirname;
const TESTS = join(SRC, '..', 'tests');

/** Generated, and not ours to shape. */
const GENERATED = ['api/schema.d.ts'];

function filesUnder(dir: string, out: string[] = []): string[] {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) filesUnder(p, out);
    else if (e.name.endsWith('.ts') || e.name.endsWith('.tsx')) out.push(p);
  }
  return out;
}

const rel = (p: string) => relative(join(SRC, '..'), p).split('\\').join('/');

// Whether this line ends a doc block — a lone closer, or a whole one-line block. Written as `//`
// rather than a doc block because spelling the closer inside one would end it early, which is how
// the first version of this file failed to parse at all.
const endsBlock = (line: string) =>
  line === '*/' || (line.startsWith('/**') && line.endsWith('*/') && line.length > 4);

/** Every place a doc block ends and the next line opens another one, as 1-based line numbers. */
export function stackedDocBlocks(text: string): number[] {
  const lines = text.split(/\r?\n/);
  const hits: number[] = [];
  for (let i = 0; i < lines.length - 1; i++) {
    if (endsBlock(lines[i].trim()) && lines[i + 1].trim().startsWith('/**')) hits.push(i + 1);
  }
  return hits;
}

describe('a doc block always has a declaration to document', () => {
  // The detector first: four shapes it must find and two it must not. Without these, a detector
  // that stopped matching would report a clean tree and read exactly like success.
  it('recognises every way two blocks can stack, and nothing else', () => {
    const hit = [
      ['/** a */', '/** b */', 'export const x = 1;'],
      ['/** a */', '/**', ' * b', ' */', 'export const x = 1;'],
      ['/**', ' * a', ' */', '/** b */', 'export const x = 1;'],
      ['/**', ' * a', ' */', '/**', ' * b', ' */', 'export const x = 1;'],
    ];
    const miss = [
      ['/**', ' * a', ' */', 'export const x = 1;', '/** b */', 'export const y = 2;'],
      ['/** a */', 'export const x = 1;', '/** b */', 'export const y = 2;'],
      ['// a', '// b', 'export const x = 1;'],
      ['/** a */', '', '/** b */', 'export const x = 1;'],
    ];
    for (const [i, c] of hit.entries()) expect(stackedDocBlocks(c.join('\n')), `hit ${i}`).toHaveLength(1);
    for (const [i, c] of miss.entries()) expect(stackedDocBlocks(c.join('\n')), `miss ${i}`).toHaveLength(0);
  });

  it('holds for every source file under web/src and web/tests', () => {
    const files = [...filesUnder(SRC), ...filesUnder(TESTS)].filter(
      (f) => !GENERATED.some((g) => rel(f).endsWith(g)),
    );
    // The floor counts what was read, not what was walked: a walk that stopped finding files would
    // otherwise pass with nothing inspected.
    expect(files.length).toBeGreaterThan(600);

    const found: string[] = [];
    for (const f of files) {
      for (const line of stackedDocBlocks(readFileSync(f, 'utf8'))) {
        found.push(`${rel(f)}:${line}`);
      }
    }
    expect(
      found,
      'a doc block is immediately followed by another: the first one documents the second block’s ' +
        'declaration, and whatever it was written for has no doc at all. Move it onto the ' +
        'declaration it describes, delete it if that declaration is gone, or make it a `//` note ' +
        'if it describes a group rather than one item.',
    ).toEqual([]);
  });
});
