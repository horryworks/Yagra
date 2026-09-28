// SPDX-License-Identifier: AGPL-3.0-only
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { codeOnly, isCommentLine, readSources, rel, sourceFiles, SRC } from './sources';

describe('sourceFiles', () => {
  it('leaves tests and declarations out unless asked', () => {
    const plain = sourceFiles().map(rel);
    expect(plain.length).toBeGreaterThan(300);
    expect(plain.some((p) => p.endsWith('.test.ts'))).toBe(false);
    expect(plain.some((p) => p.endsWith('.d.ts'))).toBe(false);
    const all = sourceFiles(SRC, { includeTests: true, declarations: true }).map(rel);
    expect(all).toContain('testSupport/sources.test.ts');
    expect(all).toContain('api/schema.d.ts');
  });

  it('skips a directory named relative to web/src, and only that one', () => {
    const skipped = sourceFiles(SRC, { skipDirs: ['api'], declarations: true }).map(rel);
    expect(skipped.some((p) => p.startsWith('api/'))).toBe(false);
    expect(skipped).toContain('services/api.ts');
  });

  it('keeps only the extensions asked for', () => {
    const tsx = sourceFiles(SRC, { exts: ['.tsx'] });
    expect(tsx.length).toBeGreaterThan(100);
    expect(tsx.every((p) => p.endsWith('.tsx'))).toBe(true);
  });
});

describe('comment lines', () => {
  it('are the // lines and the lines of a block comment', () => {
    expect(isCommentLine('  // x')).toBe(true);
    expect(isCommentLine(' * x')).toBe(true);
    expect(isCommentLine('/** x */')).toBe(true);
    expect(isCommentLine('const x = 1; // y')).toBe(false);
    expect(codeOnly('a\n// b\nc')).toBe('a\n\nc');
  });
});

/**
 * ADR-184 increment 21: the tree walk is this module's. Fourteen tests had their own; what still
 * lists a directory itself is below, each for a reason that is not "the walk".
 *
 * ⚠️ Assembled at runtime, or it would match this file.
 */
describe('no test walks the source tree itself', () => {
  const DECLARED: Record<string, string> = {
    'testSupport/sources.ts': 'the walk',
    'tsxJudgement.test.ts': 'resolves imports between the files it walks',
    'components/NodeDetail/RangeControl.test.ts': 'one directory — the panes beside it — not a tree',
    'lib/nodeKind.test.ts': 'Rust sources in yagra-common',
    'i18nPrefixes.test.ts': 'the locale files of one directory',
    'testIds.test.ts': 'the Playwright specs in tests/ui',
  };
  const NEEDLE = `${'readdir'}Sync(`;

  it('every other test reads sources through testSupport/sources', () => {
    const files = readSources(SRC, { includeTests: true });
    expect(files.length).toBeGreaterThan(500);
    const walkers = files.filter(([, src]) => src.includes(NEEDLE)).map(([p]) => p);
    expect(walkers.filter((p) => !(p in DECLARED))).toEqual([]);
    // Both ways: a declared file that stopped listing a directory leaves the list.
    expect(Object.keys(DECLARED).filter((p) => !walkers.includes(p))).toEqual([]);
    expect(readFileSync(`${SRC}/testSupport/sources.ts`, 'utf8')).toContain(NEEDLE);
  });
});
