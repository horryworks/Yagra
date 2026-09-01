// SPDX-License-Identifier: AGPL-3.0-only
// ADR-052 Inc.6: judgement that lives in a `.tsx` is judgement nothing runs.
//
// `vite.config.ts` sets `include: ['src/**/*.test.ts']`, so a `.test.tsx` is not even collected —
// that ban stays (jsdom has no layout engine, so it cannot see the bugs Tier1 exists for). What
// this guard closes is the ban's other half: the sanctioned repair is to move the judgement
// somewhere a test can reach, and until now nothing checked that anybody had.
//
// **The rule is about reach, not about location.** Two shapes already satisfy it in this tree and
// both are correct:
//
//   1. move the helper to a neighbouring `.ts` (`lib/nodeTree.ts`, `NodeDetail/linkMode.ts`, … —
//      the large majority), or
//   2. `export` it from the `.tsx` and import that module from a `.test.ts` — right for a helper
//      that cannot be separated from the module it serves (`troubleshoot/report/registry.tsx`
//      holds components; `FlowSankey.tsx`'s builder feeds its own SVG).
//
//      ⚠️ Shape 2 holds only while the module touches no DOM at import time. `RangeControl.test.ts`
//      says so at the top; a new user of shape 2 owes the same line.
//
// 🚨 **Two detector bugs are worth knowing about, because both made it under-report.** The first
// version counted `>` in a parameter's `=>` as a closing bracket, so any helper taking a callback
// had its body read as the first `{` it could find — `eventColumns` measured as one line. The
// second only looked for JSX in the `return`, so a factory that builds JSX into a descriptor
// looked pure. A checker that under-reports is indistinguishable from a clean tree, which is why
// the floors below count what was *inspected* rather than what was walked.
import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative, resolve, dirname } from 'node:path';

const SRC = join(__dirname);

function filesUnder(dir: string, ext: string, out: string[] = []): string[] {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) filesUnder(p, ext, out);
    else if (e.name.endsWith(ext)) out.push(p);
  }
  return out;
}

const rel = (p: string) => relative(SRC, p).split('\\').join('/');

/** A top-level helper found in a `.tsx`: its name, whether it is exported, and its body. */
interface Helper {
  file: string;
  name: string;
  exported: boolean;
  body: string;
}

/**
 * The body of a `function name(...)` declaration at column 0.
 *
 * Parameter lists are matched on **parentheses only**. Counting `<`/`>` as brackets breaks on
 * `(id: string) => string`, where the arrow's `>` closes a depth that was never opened — the first
 * version of this did exactly that and read a 63-line function as one line.
 */
function functionBodies(src: string): Helper[] {
  const out: Helper[] = [];
  const re = /^(export\s+)?function\s+([A-Za-z_$][\w$]*)\s*[(<]/gm;
  let m: RegExpExecArray | null;
  while ((m = re.exec(src))) {
    let i = src.indexOf('(', m.index);
    if (i < 0) continue;
    let depth = 0;
    for (; i < src.length; i++) {
      if (src[i] === '(') depth++;
      else if (src[i] === ')') {
        depth--;
        if (depth === 0) break;
      }
    }
    const brace = src.indexOf('{', i);
    if (brace < 0) continue;
    let b = 0;
    let end = brace;
    for (let j = brace; j < src.length; j++) {
      if (src[j] === '{') b++;
      else if (src[j] === '}') {
        b--;
        if (b === 0) {
          end = j;
          break;
        }
      }
    }
    out.push({ file: '', name: m[2], exported: !!m[1], body: src.slice(brace, end + 1) });
  }
  return out;
}

/** A top-level `const name = (…) => …`. The body is taken to the next column-0 declaration, which
 *  over-reads rather than under-reads — the JSX test below only needs to see enough.
 *
 *  🚨 The `=>` must be found **on the signature's own line**. Letting the scan cross a newline made
 *  every `const x: SomeType = {` whose object literal contained an arrow anywhere read as a
 *  function: `troubleshoot/report/registry.tsx`'s fifteen report descriptors are data, and all
 *  fifteen were reported as untested helpers. */
function arrowBodies(src: string): Helper[] {
  const out: Helper[] = [];
  const re = /^(export )?const ([a-z][\w$]*)(?::[^=\n]+)? = (?:<[^>\n]*>\s*)?\(?[^=;\n]*?\)?\s*=>/gm;
  let m: RegExpExecArray | null;
  while ((m = re.exec(src))) {
    const rest = src.slice(m.index);
    const nextTop = rest.slice(1).search(/\n(?:export )?(?:const|function|interface|type|class) /);
    out.push({
      file: '',
      name: m[2],
      exported: !!m[1],
      body: nextTop < 0 ? rest : rest.slice(0, nextTop + 1),
    });
  }
  return out;
}

/** Anything that renders. A descriptor factory that builds JSX into its rows is a component's
 *  business and belongs in the `.tsx`; only the judgement around it has to move. */
const RENDERS = /<\/[A-Za-z]|\/>|<[A-Z][A-Za-z]*[\s/>]/;

function helpersIn(file: string): Helper[] {
  const src = readFileSync(file, 'utf8');
  return [...functionBodies(src), ...arrowBodies(src)]
    .filter((h) => /^[a-z]/.test(h.name) && !/^use[A-Z]/.test(h.name) && !RENDERS.test(h.body))
    .map((h) => ({ ...h, file }));
}

/**
 * Helpers this increment did not move, each with the reason it stays where it is.
 *
 * ⚠️ **"Not done yet" is not a reason.** An exemption list that accumulates unfinished work is a
 * backlog wearing a checker's clothes: it passes forever and nobody reads it. Every line here says
 * why a *test* cannot or should not reach the helper, and the list is meant to stay short.
 */
const HELPERS_WITHOUT_A_TEST: Record<string, string> = {
  'components/ui/icons.tsx::base':
    'the shared SVG attribute defaults for a file that is nothing but icon components. It has no ' +
    'branch: it spreads four constants and then `...props`, so the only rule is "the caller wins", ' +
    'and TypeScript already gives that. A test here would restate the four literals, which is the ' +
    'kind of assertion that fails on every legitimate edit and proves nothing.',
};

/** Every `.test.ts`, with the set of modules it imports (resolved against the filesystem). */
function testImports(): { file: string; text: string; imports: Set<string> }[] {
  return filesUnder(SRC, '.test.ts').map((f) => {
    const text = readFileSync(f, 'utf8');
    const imports = new Set<string>();
    for (const m of text.matchAll(/from '(\.[^']+)'/g)) {
      imports.add(resolve(dirname(f), m[1]) + '.tsx');
    }
    return { file: f, text, imports };
  });
}

describe('every pure helper in a .tsx is reachable from a test', () => {
  const tsx = filesUnder(SRC, '.tsx').filter((f) => !f.includes('.test.'));
  const helpers = tsx.flatMap(helpersIn);
  const tests = testImports();

  it('inspected the sources it is supposed to be reading', () => {
    // The failure a guard must never have: a broken parser reports "nothing wrong", which looks
    // exactly like a clean tree (`floor-must-count-what-was-checked`).
    //
    // ⚠️ **The candidate floor is deliberately low, and that is not laziness.** The first version
    // set it at 40 — the number measured on the day — and it failed *while the work was going
    // well*, because every helper correctly moved to a `.ts` is one fewer candidate here. A floor
    // that has to be lowered each time someone does the right thing is a ratchet pointing the
    // wrong way, and the third person to hit it deletes it. What actually proves the detector is
    // alive is the recognition test below, which names a helper that exists and a component that
    // must not be one; this number only has to prove the walk was not empty.
    expect(tsx.length).toBeGreaterThan(150);
    expect(helpers.length).toBeGreaterThan(10);
    // …and it must not be reading itself: this file is a `.test.ts`, so it is outside the walk by
    // construction. Assert that rather than trusting it (`self-matching-needle-has-two-directions`).
    expect(tsx.some((f) => f.endsWith('tsxJudgement.test.ts'))).toBe(false);
  });

  it('still recognises a helper, and still ignores a component', () => {
    // The accept side. Without it, a detector that matched nothing would satisfy every assertion
    // below (`rejection-only-tests-pass-when-everything-rejects`).
    const sankey = helpersIn(join(SRC, 'components/NodeDetail/FlowSankey.tsx'));
    expect(sankey.map((h) => h.name)).toContain('buildSankey');
    // …and the reject side: a file of components yields no helpers at all.
    const rows = helpersIn(join(SRC, 'widgets/AlertRows.tsx'));
    expect(rows.map((h) => h.name)).not.toContain('AlertRows');
  });

  it('reads a multi-line signature whose parameter is a callback', () => {
    // The bug that made the first version under-report: `(id: string) => string` in a parameter
    // list closed a bracket depth that was never opened, and the body was read as one line.
    const cols = functionBodies(
      readFileSync(join(SRC, 'components/EventLog/eventColumns.tsx'), 'utf8'),
    );
    const ec = cols.find((h) => h.name === 'eventColumns');
    expect(ec).toBeDefined();
    expect(ec!.body.split('\n').length).toBeGreaterThan(20);
    // …and it renders, so it is correctly not a candidate.
    expect(RENDERS.test(ec!.body)).toBe(true);
  });

  it('names no helper a test cannot reach', () => {
    const unreachable: string[] = [];
    for (const h of helpers) {
      const key = `${rel(h.file)}::${h.name}`;
      if (key in HELPERS_WITHOUT_A_TEST) continue;
      const named = tests.some(
        (t) => t.imports.has(h.file) && new RegExp(`\\b${h.name}\\b`).test(t.text),
      );
      if (!h.exported) {
        unreachable.push(`${key} — not exported, so no test can import it`);
      } else if (!named) {
        unreachable.push(`${key} — exported, but no .test.ts that imports this module names it`);
      }
    }
    expect(unreachable.sort()).toEqual([]);
  });

  it('carries a reason for every exemption, and none of them is stale', () => {
    const live = new Set(helpers.map((h) => `${rel(h.file)}::${h.name}`));
    for (const [key, why] of Object.entries(HELPERS_WITHOUT_A_TEST)) {
      expect(why.length, `${key} has no reason`).toBeGreaterThan(20);
      // A helper that was moved or deleted must not leave its exemption behind, quietly widening
      // the rule for whatever is written next in that file.
      expect(live.has(key), `${key} is exempted but no longer exists`).toBe(true);
    }
  });
});
