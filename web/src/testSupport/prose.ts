// SPDX-License-Identifier: AGPL-3.0-only
// Measuring the prose in the locale files and on hover, for `proseBudget.test.ts` (ADR-200).
//
// "Prose" is a string of five or more English words. The unit that is budgeted is its length in
// characters, summed per namespace — the mass, not the count, because one 400-character paragraph
// costs a reader more than ten five-word labels.
//
// Imported only by `*.test.ts`.

import { flattenStrings, lookup, type Json } from './locales';
import { codeOnly } from './sources';

/** Namespaces that are measured by nothing here: generated from Rust, and read as data. */
export const NOT_MEASURED = ['metricMeanings'];

/** Words in an English string, with `<tag>` and `{{placeholder}}` read as spaces. */
export function wordCount(s: string): number {
  return s
    .replace(/<[^>]*>/g, ' ')
    .replace(/\{\{[^}]*\}\}/g, ' ')
    .split(/\s+/)
    .filter(Boolean).length;
}

export const isProse = (en: string) => wordCount(en) >= 5;

/** The Japanese string for an English key; `x_one` reads JA's `x_other` (JA has no `_one`). */
export function jaFor(ja: Json, key: string): string {
  const v = lookup(ja, key);
  if (typeof v === 'string') return v;
  const other = lookup(ja, key.replace(/_one$/, '_other'));
  return typeof other === 'string' ? other : '';
}

/** `[en, ja]` prose mass of one namespace. */
export function proseMass(en: Json, ja: Json): [number, number] {
  let e = 0;
  let j = 0;
  for (const [key, v] of Object.entries(flattenStrings(en))) {
    if (!isProse(v)) continue;
    e += v.length;
    j += jaFor(ja, key).length;
  }
  return [e, j];
}

/** Every `ns:key` whose English is over `enCap` characters or whose Japanese is over `jaCap`. */
export function longKeys(
  locales: Record<string, { en: Json; ja: Json }>,
  enCap = 200,
  jaCap = 120,
): string[] {
  const out: string[] = [];
  for (const [ns, { en, ja }] of Object.entries(locales)) {
    if (NOT_MEASURED.includes(ns)) continue;
    for (const [key, v] of Object.entries(flattenStrings(en))) {
      if (v.length > enCap || jaFor(ja, key).length > jaCap) out.push(`${ns}:${key}`);
    }
  }
  return out.sort();
}

/** Every literal key read straight into a `title` attribute: `title={t('k'…`. */
export function hoverKeysIn(src: string): string[] {
  return [...src.matchAll(/\btitle=\{\s*t\(\s*(['"])([^'"]+)\1/g)].map((m) => m[2]);
}

/** Every `<PageHeader …>` opening tag in a source file, comment lines removed. Braces are tracked so
 *  an arrow's `>` inside `note={…}` does not end the tag. */
export function pageHeaderTags(src: string): string[] {
  const code = codeOnly(src);
  const out: string[] = [];
  for (const m of code.matchAll(/<PageHeader\b/g)) {
    let depth = 0;
    let i = m.index;
    for (; i < code.length; i++) {
      const c = code[i];
      if (c === '{') depth++;
      else if (c === '}') depth--;
      else if (c === '>' && depth === 0) break;
    }
    out.push(code.slice(m.index, i + 1));
  }
  return out;
}

/** The literal key a page header's `note={t('…')}` reads, or null for any other note. */
export function noteKeyOf(tag: string): string | null {
  return /\bnote=\{\s*t\(\s*(['"])([^'"]+)\1\s*\)\s*\}/.exec(tag)?.[2] ?? null;
}
