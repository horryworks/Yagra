// SPDX-License-Identifier: AGPL-3.0-only
// Guards the testid map (ADR-052 決定 4). Two directions, and the first one is the load-bearing
// half: it is a **ratchet**, not a description. It passes today because there is one testid and it
// comes from the map — and it goes on passing only while that stays true. The moment somebody
// writes `data-testid="node-row"` inline, the trap this map exists to prevent has started, and
// this is the only thing that will say so.

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { TEST_IDS } from './testIds';

const SRC = join(process.cwd(), 'src');

function sourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) sourceFiles(full, out);
    else if (/\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name)) out.push(full);
  }
  return out;
}

describe('test ids', () => {
  it('are never written inline — every one comes from the map', () => {
    // The needle is assembled at runtime. A literal `data-testid="` in this file's own source
    // would match itself the moment the check ever scanned test files, and the test would then
    // fail forever for the wrong reason (`reports/guards.rs` has the same note for the same reason).
    const attr = ['data', 'testid'].join('-');
    const inline = new RegExp(`${attr}\\s*=\\s*["'\`]`);
    // Comment lines are skipped — the first version of this test failed on `testIds.ts`'s own
    // doc comment, which quotes the attribute while explaining why not to write it. A rule that
    // cannot be described in prose without tripping itself gets edited into silence.
    const isComment = (line: string) => /^\s*(\/\/|\*|\/\*)/.test(line);
    const offenders = sourceFiles(SRC)
      .filter((f) =>
        readFileSync(f, 'utf8')
          .split('\n')
          .some((line) => !isComment(line) && inline.test(line)),
      )
      .map((f) => f.slice(SRC.length + 1));
    expect(offenders, 'put the string in src/testIds.ts and reference it from there').toEqual([]);
  });

  it('are all used by something', () => {
    const sources = sourceFiles(SRC)
      .filter((f) => !f.endsWith(join('src', 'testIds.ts')))
      .map((f) => readFileSync(f, 'utf8'));
    const specs = readdirSync(join(process.cwd(), 'tests', 'ui'))
      .filter((f) => f.endsWith('.ts'))
      .map((f) => readFileSync(join(process.cwd(), 'tests', 'ui', f), 'utf8'));
    // Each id must be reachable from BOTH sides — an id nothing renders is a dead string, and an
    // id no test looks for is a hook nobody uses. Both are the map growing without paying for it.
    // A spec may name it either way: through the constant (preferred) or as the raw string.
    for (const [key, id] of Object.entries(TEST_IDS)) {
      expect(
        sources.some((s) => s.includes(`TEST_IDS.${key}`)),
        `${key} is rendered by nothing`,
      ).toBe(true);
      expect(
        specs.some((s) => s.includes(`TEST_IDS.${key}`) || s.includes(id)),
        `${key} ("${id}") is asserted by no spec`,
      ).toBe(true);
    }
  });

  it('read as what the operator sees, not as internals', () => {
    for (const id of Object.values(TEST_IDS)) {
      expect(id, 'testids ship in the production bundle — keep them descriptive').toMatch(
        /^[a-z][a-z0-9-]*$/,
      );
    }
  });

  it('has a source file that exists where the guard looks', () => {
    // Cheap insurance against the scan silently covering nothing (a moved `src/`, a changed cwd).
    expect(statSync(SRC).isDirectory()).toBe(true);
    expect(sourceFiles(SRC).length).toBeGreaterThan(100);
  });
});
