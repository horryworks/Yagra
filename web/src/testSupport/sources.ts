// SPDX-License-Identifier: AGPL-3.0-only
// Reading the WebUI's own source, for the tests that guard it (ADR-184).
//
// Fourteen guard tests each carried their own directory walk — the same `readdirSync` recursion,
// the same `.test.` exclusion, the same backslash fix for Windows paths — and they had drifted on
// the details that decide what a guard can see (`.ts` only, `.tsx` only, tests in or out). This is
// the one walk. What stays in each test is what makes the guard mean something: its needle, its
// exemptions, and **its floor** — the assertion that it found the sources it expected, so a moved
// directory fails the guard instead of emptying it.
//
// Still listing a directory themselves, on purpose — `sources.test.ts` holds this list:
// `tsxJudgement.test.ts` (it resolves imports between the files it walks), `RangeControl.test.ts`
// (one directory, not a tree), `nodeKind.test.ts` (Rust sources), and the one-directory listings
// in `testSupport/locales.ts` (the locale files) and `testIds.test.ts` (`tests/ui`).
//
// Imported only by `*.test.ts`. It uses `node:fs`, so a component importing it would not build.

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import type { TFunction } from 'i18next';

/** `web/src`. */
export const SRC = join(__dirname, '..');

/** A path relative to `web/src`, with forward slashes on every platform. */
export function rel(p: string): string {
  return p.slice(SRC.length + 1).replace(/\\/g, '/');
}

export interface SourceOptions {
  /** File extensions to keep, with the dot. Default `.ts` and `.tsx`. */
  exts?: readonly string[];
  /** Keep `*.test.ts` / `*.test.tsx`. Default false: a test that quotes a needle is not a copy. */
  includeTests?: boolean;
  /** Directories to skip, relative to `web/src` (`'api'`, `'services'`). */
  skipDirs?: readonly string[];
  /** Keep `.d.ts` files. Default false: a declaration builds nothing. Several guards were written
   *  with them in, and keep them in so this move changed no guard's file set. */
  declarations?: boolean;
}

/** Every source file under `root` (default `web/src`), absolute paths, in directory order. */
export function sourceFiles(root: string = SRC, opts: SourceOptions = {}): string[] {
  const exts = opts.exts ?? ['.ts', '.tsx'];
  const skip = new Set(opts.skipDirs ?? []);
  const walk = (dir: string): string[] =>
    readdirSync(dir).flatMap((e) => {
      const p = join(dir, e);
      if (statSync(p).isDirectory()) return skip.has(rel(p)) ? [] : walk(p);
      if (!exts.some((x) => e.endsWith(x))) return [];
      if (!opts.includeTests && /\.test\.tsx?$/.test(e)) return [];
      if (!opts.declarations && e.endsWith('.d.ts')) return [];
      return [p];
    });
  return walk(root);
}

/** `[relative path, contents]` for each of {@link sourceFiles}. */
export function readSources(root: string = SRC, opts: SourceOptions = {}): [string, string][] {
  return sourceFiles(root, opts).map((p) => [rel(p), readFileSync(p, 'utf8')]);
}

/** A line that only describes code — a `//` line or a line inside a block comment. A guard that
 *  skips these can name its needle in a doc comment without matching it. */
export function isCommentLine(line: string): boolean {
  const t = line.trimStart();
  return t.startsWith('//') || t.startsWith('*') || t.startsWith('/*');
}

/** The source with its comment lines blanked (line numbers kept). */
export function codeOnly(src: string): string {
  return src
    .split('\n')
    .map((l) => (isCommentLine(l) ? '' : l))
    .join('\n');
}

/** A `t` that returns its key — for asserting *which* key a pure helper asks for. */
export const fakeT = ((key: string) => key) as unknown as TFunction;
