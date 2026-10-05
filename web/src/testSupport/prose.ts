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

/** Every `<PageHeader …>` opening tag in a source file, comment lines removed. */
export function pageHeaderTags(src: string): string[] {
  return openingTags(src, 'PageHeader');
}

/** Every `<Name …>` opening tag in a source file, comment lines removed. Braces are tracked so an
 *  arrow's `>` inside `note={…}` does not end the tag. */
export function openingTags(src: string, name: string): string[] {
  const code = codeOnly(src);
  const out: string[] = [];
  for (const m of code.matchAll(new RegExp(`<${name}\\b`, 'g'))) {
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

// ── ADR-200 G8–G10 ─────────────────────────────────────────────────────────────────────────────

/** Every `ns:key` whose last segment is `info` — the text an ⓘ or a pressable label opens. */
export function infoKeys(locales: Record<string, { en: Json }>): string[] {
  const out: string[] = [];
  for (const [ns, { en }] of Object.entries(locales)) {
    for (const key of Object.keys(flattenStrings(en))) {
      if (/(^|\.)info$/.test(key)) out.push(`${ns}:${key}`);
    }
  }
  return out.sort();
}

/** Every namespaced `.info` key quoted in code (comment lines excluded), once per occurrence. */
export function infoKeyLiterals(src: string): string[] {
  const re = /(['"`])([A-Za-z][\w-]*:[\w.-]+\.info)\1/g;
  return [...codeOnly(src).matchAll(re)].map((m) => m[2]);
}

/** The literal value of every `infoKey="…"` / `infoKey={'…'}` attribute. */
export function infoKeyAttrs(src: string): string[] {
  const re = /\binfoKey=(?:\{\s*)?(['"`])([^'"`]+)\1/g;
  return [...codeOnly(src).matchAll(re)].map((m) => m[2]);
}

/** Sentences in a string, counted by their ends: `.` `!` `?` before a space or the end, and the
 *  Japanese full stop (U+3002). */
export function sentenceCount(s: string): number {
  return (s.match(/[.!?](?=\s|$)|。/g) ?? []).length;
}

/** The ⓘ and pressable labels one file draws: `<InfoTip`, plus `<Field` with an `infoKey` (which
 *  draws one inside), and `<InfoPress`. */
export function infoSites(src: string): { tip: number; press: number } {
  const fields = openingTags(src, 'Field').filter((tag) => /\binfoKey=/.test(tag)).length;
  return {
    tip: openingTags(src, 'InfoTip').length + fields,
    press: openingTags(src, 'InfoPress').length,
  };
}

/** Every `ns:key` whose English or Japanese spells a menu path with `▸`. */
export function pointerKeys(locales: Record<string, { en: Json; ja: Json }>): string[] {
  const out: string[] = [];
  for (const [ns, { en, ja }] of Object.entries(locales)) {
    const jaFlat = flattenStrings(ja);
    for (const [key, v] of Object.entries(flattenStrings(en))) {
      if (v.includes('▸') || (jaFlat[key] ?? '').includes('▸')) out.push(`${ns}:${key}`);
    }
  }
  return out.sort();
}

/** Static hints in one file: a `<FieldHint` without `error`, a `form-hint` class on a line with no
 *  `form-hint-error`, and every `modal-hint` class. */
export function hintSites(src: string): { fieldHint: number; formHint: number; modalHint: number } {
  const code = codeOnly(src);
  const fieldHint = openingTags(src, 'FieldHint').filter((tag) => !/\berror\b/.test(tag)).length;
  let formHint = 0;
  for (const line of code.split('\n')) {
    if (line.includes('form-hint-error')) continue;
    formHint += (line.match(/\bform-hint\b(?!-)/g) ?? []).length;
  }
  const modalHint = (code.match(/\bmodal-hint\b(?!-)/g) ?? []).length;
  return { fieldHint, formHint, modalHint };
}
