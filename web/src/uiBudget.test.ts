// SPDX-License-Identifier: AGPL-3.0-only
// ADR-246: every stylesheet and component is held to the spacing scale and the shared layout
// parts, by ceilings that only go down.
//
// WHY A CEILING AND NOT A BAN. On the day this landed the tree held 1,555 raw spacing values and
// 471 uses of the class names the layout parts replace, across 127 and 54 files. A ban would have
// meant fixing all of them before the check could exist; a ceiling per file freezes them instead,
// so a NEW raw value fails today while the old ones leave screen by screen (ADR-246 Inc.3-7).
// The same shape as `proseBudget.test.ts`, with one difference that matters: the counting lives in
// `scripts/ui-rules.mjs`, because the edit hook (`.claude/hooks/ui-guard.js`) must give the same
// answer as this test about the same file. A second implementation in either place would let the
// hook pass what the test then fails, or the reverse - which is exactly the drift this ADR is for.
//
// What a failure here asks for:
//   over      you added a raw value (or a legacy class, or stylesheet lines) - use a token or a part
//   lower     you removed some - lower the ceiling to the new count in the same change
//             (`node scripts/ui-rules.mjs --lower` does it for every file, and never raises one)
//   unlisted  a new page-local stylesheet - list it in `pageCss` with the reason it exists
//   allow     a `ui-allow:` marker with no reason, or on a line with nothing to allow

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  METRICS,
  SPACE_STEPS,
  compare,
  describe as describeProblem,
  isBlocking,
  isPageCss,
  measure,
  measureCss,
  measureTsx,
  suggest,
  type Budget,
} from '../scripts/ui-rules.mjs';
import budgetJson from './styles/uiBudget.json';
import { SRC, readSources } from './testSupport/sources';

const budget = budgetJson as Budget;
const files = readSources(SRC, { exts: ['.css', '.tsx'], skipDirs: ['api'] });

describe('the UI budget (ADR-246)', () => {
  it('holds every file within its ceilings, and no ceiling far above its count', () => {
    const problems: string[] = [];
    let inspected = 0;
    for (const [rel, text] of files) {
      const r = measure(rel, text);
      if (!r) continue;
      inspected += r.inspected;
      for (const p of compare(rel, r, budget)) problems.push(describeProblem(rel, p));
    }
    expect(problems, problems.join('\n\n')).toEqual([]);
    // The floor proves the walk read something; it counts what was inspected, which a migration
    // does not shrink (a raw value replaced by a token is still a declaration looked at).
    expect(inspected).toBeGreaterThan(2500);
    expect(files.length).toBeGreaterThan(250);
  });

  it('names only files that exist, with whole positive ceilings', () => {
    const present = new Set(files.map(([rel]) => rel));
    const stale: string[] = [];
    for (const m of METRICS) {
      for (const [rel, n] of Object.entries(budget[m] ?? {})) {
        if (!present.has(rel)) stale.push(`${m}: ${rel} (no such file)`);
        if (!Number.isInteger(n) || n <= 0) stale.push(`${m}: ${rel} = ${n} (drop a zero entry)`);
      }
    }
    for (const [rel, e] of Object.entries(budget.pageCss ?? {})) {
      if (!present.has(rel)) stale.push(`pageCss: ${rel} (no such file)`);
      if (!isPageCss(rel)) stale.push(`pageCss: ${rel} (not a page-local stylesheet)`);
      if (!Number.isInteger(e.lines) || e.lines <= 0) stale.push(`pageCss: ${rel} lines = ${e.lines}`);
    }
    expect(stale, stale.join('\n')).toEqual([]);
  });

  it('has the same scale as tokens.css', () => {
    // Two spellings of one scale: the tokens a stylesheet reads, and the steps the checker
    // suggests. A step added to one and not the other would have the hook suggest a token that
    // does not exist, or never suggest one that does.
    const tokens = readFileSync(join(SRC, 'styles', 'tokens.css'), 'utf8');
    const declared = [...tokens.matchAll(/(--space-[\d-]+):\s*(\d+)px;/g)].map(
      ([, name, px]) => [Number(px), name] as [number, string],
    );
    expect(declared.sort((a, b) => a[0] - b[0])).toEqual(SPACE_STEPS);
  });
});

describe('ui-rules.mjs recognises what it is for', () => {
  // A checker whose healthy answer is "nothing found" must be shown to find something, or a
  // detector that stopped matching would read as a clean tree.
  it('finds raw spacing, radius, colour and font sizes in a stylesheet', () => {
    const r = measureCss(
      'pages/Fixture.css',
      [
        '.a { gap: 10px; padding: 6px var(--space-2); }',
        '.b { border-radius: 8px; margin-top: -4px; }',
        '.c { color: #fff; background: rgba(0, 0, 0, 0.2); border: 1px solid white; }',
        '.d { font-size: 11px; --pane-gap: 3px; }',
        '.e { padding: calc(100% - 12px); }',
      ].join('\n'),
    );
    expect(r.spacing.map((v) => v.raw)).toEqual(['10px', '6px', '8px', '-4px', '3px', '12px']);
    expect(r.color.map((v) => v.raw)).toEqual(['#fff', 'rgba(', 'white']);
    expect(r.fontSize.map((v) => v.raw)).toEqual(['11px']);
    expect(r.lines).toBe(5);
  });

  it('leaves tokens, zero, layout sizes, comments and fallbacks alone', () => {
    const r = measureCss(
      'pages/Fixture.css',
      [
        '/* gap: 10px; color: #fff */',
        '.a { gap: var(--space-2); margin: 0 auto; padding: calc(var(--space-2) * -1); }',
        '.b { width: 10px; height: 30px; top: 4px; border-radius: 50%; }',
        '.c { padding: var(--space-x, 10px); background: url("data:image/svg+xml,%23fff"); }',
        '.d { color: var(--text-primary); border-radius: var(--radius-pill); }',
        '.e:hover { white-space: nowrap; }',
      ].join('\n'),
    );
    expect(r.spacing).toEqual([]);
    expect(r.color).toEqual([]);
    expect(r.fontSize).toEqual([]);
  });

  it('does not count the token definitions themselves, nor a shared stylesheet as page-local', () => {
    const tokens = measureCss('styles/tokens.css', ':root { --space-2: 8px; --radius: 6px; }');
    expect(tokens.spacing).toEqual([]);
    expect(tokens.lines).toBeNull();
    expect(isPageCss('pages/FooPage.css')).toBe(true);
    expect(isPageCss('components/NodeDetail/NodeDetail.css')).toBe(true);
    expect(isPageCss('components/ui/Card.css')).toBe(false);
    expect(isPageCss('styles/table.css')).toBe(false);
    expect(isPageCss('index.css')).toBe(false);
  });

  it('drops an allowed line, and refuses an allow with no reason or nothing to allow', () => {
    const r = measureCss(
      'pages/Fixture.css',
      [
        '.a { font-size: 9px; /* ui-allow: SVG user units */ }',
        '.b { gap: 2px; /* ui-allow: */ }',
        '.c { gap: var(--space-1); /* ui-allow: nothing here */ }',
      ].join('\n'),
    );
    expect(r.fontSize).toEqual([]);
    expect(r.spacing).toEqual([]);
    expect(r.allowProblems.map((a) => a.line)).toEqual([2, 3]);
  });

  it('finds literal spacing in an inline style and the legacy class names in a component', () => {
    const src = [
      'export function A({ depth }: { depth: number }) {',
      '  return (',
      "    <div className=\"modal-field x\" style={{ marginTop: 8, padding: '4px 0' }}>",
      "      <span style={{ marginTop: 'var(--space-2)', paddingLeft: depth * INDENT }} />",
      '      <span className={`form-row ${x}`} />',
      '    </div>',
      '  );',
      '}',
    ].join('\n');
    const page = measureTsx('pages/Fixture.tsx', src);
    expect(page.inlineSpacing.map((v) => [v.line, v.raw])).toEqual([
      [3, '8'],
      [3, '4px'],
    ]);
    expect(page.legacy.map((v) => v.raw)).toEqual(['modal-field', 'form-row']);
    // The shared parts may still render the classes they are about to retire.
    expect(measureTsx('components/ui/Field.tsx', src).legacy).toEqual([]);
  });

  it('suggests the nearest steps, and the right radius', () => {
    expect(suggest({ prop: 'gap', raw: '6px' })).toBe('var(--space-1-5)');
    expect(suggest({ prop: 'gap', raw: '10px' })).toBe(
      'var(--space-2) inside a component, var(--space-3) between blocks',
    );
    expect(suggest({ prop: 'margin-top', raw: '-8px' })).toBe('calc(-1 * var(--space-2))');
    expect(suggest({ prop: 'border-radius', raw: '6px' })).toBe('var(--radius)');
    expect(suggest({ prop: 'border-radius', raw: '11px' })).toBe(
      'var(--radius-pill) if round at its ends, else var(--radius)',
    );
    expect(suggest({ prop: 'border-radius', raw: '999px' })).toBe('var(--radius-pill)');
    expect(suggest({ prop: 'marginTop', raw: '8' })).toBe('var(--space-2)');
  });

  it('compares against the budget in both directions', () => {
    const r = measureCss('pages/Fixture.css', '.a { gap: 10px; }\n');
    const fits: Budget = { spacing: { 'pages/Fixture.css': 1 }, pageCss: { 'pages/Fixture.css': { lines: 1, why: 'x' } } };
    expect(compare('pages/Fixture.css', r, fits)).toEqual([]);
    const tight: Budget = { pageCss: { 'pages/Fixture.css': { lines: 1, why: 'x' } } };
    expect(compare('pages/Fixture.css', r, tight).map((p) => p.kind)).toEqual(['over']);
    const loose: Budget = { spacing: { 'pages/Fixture.css': 11 }, pageCss: { 'pages/Fixture.css': { lines: 1, why: 'x' } } };
    const lower = compare('pages/Fixture.css', r, loose);
    expect(lower.map((p) => p.kind)).toEqual(['lower']);
    expect(lower.every((p) => !isBlocking(p))).toBe(true);
    expect(compare('pages/Fixture.css', r, { spacing: { 'pages/Fixture.css': 1 } }).map((p) => p.kind)).toEqual(['unlisted']);
  });
});
