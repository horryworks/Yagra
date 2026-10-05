// SPDX-License-Identifier: AGPL-3.0-only
// Reading the locale files, for the tests that guard them (ADR-150, ADR-200).
//
// `i18nPrefixes.test.ts` held these first. ADR-200 added two more guards over the same files
// (`i18nKeys.test.ts`, `proseBudget.test.ts`), and a test may not import another test, so the
// readers moved here. Each guard keeps its own floor.
//
// Imported only by `*.test.ts`. It uses `node:fs`, so a component importing it would not build.

import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './sources';

export type Json = Record<string, unknown>;

export const LOCALES = join(SRC, 'locales');

/** Every namespace, loaded from disk so a new locale file is covered without editing a test. */
export function loadLocales(): Record<string, { en: Json; ja: Json }> {
  const out: Record<string, { en: Json; ja: Json }> = {};
  for (const f of readdirSync(join(LOCALES, 'en'))) {
    if (!f.endsWith('.json')) continue;
    const ns = f.slice(0, -'.json'.length);
    out[ns] = {
      en: JSON.parse(readFileSync(join(LOCALES, 'en', f), 'utf8')) as Json,
      ja: JSON.parse(readFileSync(join(LOCALES, 'ja', f), 'utf8')) as Json,
    };
  }
  return out;
}

/** Every string leaf of a namespace, keyed by its dotted path (`a.b.c`). */
export function flattenStrings(ns: Json, prefix = ''): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(ns)) {
    const path = prefix === '' ? k : `${prefix}.${k}`;
    if (typeof v === 'string') out[path] = v;
    else if (v && typeof v === 'object' && !Array.isArray(v)) {
      Object.assign(out, flattenStrings(v as Json, path));
    }
  }
  return out;
}

export interface CallSite {
  file: string;
  line: number;
  prefix: string;
}

/**
 * The literal prefix of every runtime-built key in one source text: `t(`PREFIX${…` and
 * `i18n.t(`PREFIX${…`. A key that starts with `${` has no literal prefix and is not a call site
 * here — nothing can be said about it without evaluating the program.
 */
export function prefixesIn(src: string, file: string): CallSite[] {
  const out: CallSite[] = [];
  const re = /\bt\(`([A-Za-z0-9_:.-]+)\$\{/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(src))) {
    const before = src.slice(0, m.index);
    // A call site quoted in a comment is prose about code, not code: `password.ts` explains the
    // key it deliberately does NOT build, and that explanation must not be held to the locale.
    const lineText = before.slice(before.lastIndexOf('\n') + 1).trimStart();
    if (lineText.startsWith('//') || lineText.startsWith('*') || lineText.startsWith('/*'))
      continue;
    const line = before.split('\n').length;
    out.push({ file, line, prefix: m[1] });
  }
  return out;
}

/** Resolve a dotted key path against a namespace object; undefined when any hop is missing. */
export function lookup(ns: Json, path: string): unknown {
  return path.split('.').reduce<unknown>((cur, part) => {
    if (cur && typeof cur === 'object' && part in (cur as Json)) return (cur as Json)[part];
    return undefined;
  }, ns);
}

/**
 * Does `rest` (the prefix with any namespace stripped) point at something in this namespace?
 *
 * The prefix is cut at its last dot: everything before it must resolve to an object, and when
 * something follows the dot — `afterwards.item` for `item1`/`item2` — at least one key under that
 * object must start with it.
 */
export function existsIn(ns: Json, rest: string): boolean {
  const cut = rest.lastIndexOf('.');
  const parent = cut < 0 ? '' : rest.slice(0, cut);
  const stem = cut < 0 ? rest : rest.slice(cut + 1);
  const obj = parent === '' ? ns : lookup(ns, parent);
  if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return false;
  if (stem === '') return true;
  return Object.keys(obj as Json).some((k) => k.startsWith(stem));
}

/**
 * Whether a prefix exists in BOTH locales — in the namespace it names, or in any one namespace when
 * it names none. The same namespace must satisfy both locales: EN having it under `nodes` and JA
 * under `common` is two different bugs, not a match.
 */
export function prefixExists(
  prefix: string,
  locales: Record<string, { en: Json; ja: Json }>,
): boolean {
  const colon = prefix.indexOf(':');
  const candidates =
    colon < 0 ? Object.values(locales) : [locales[prefix.slice(0, colon)]].filter(Boolean);
  const rest = colon < 0 ? prefix : prefix.slice(colon + 1);
  return candidates.some(({ en, ja }) => existsIn(en, rest) && existsIn(ja, rest));
}

/** The namespaces one source file asked for; `null` when it asked for none (it is handed a `t`). */
export function namespacesOf(src: string): string[] | null {
  const out = new Set<string>();
  const re = /\buseTranslation\(\s*(\[[^\]]*\]|'[^']*'|"[^"]*")?/g;
  let m: RegExpExecArray | null;
  let seen = false;
  while ((m = re.exec(src))) {
    seen = true;
    if (!m[1]) out.add('common');
    else for (const q of m[1].matchAll(/['"]([^'"]+)['"]/g)) out.add(q[1]);
  }
  return seen ? [...out] : null;
}

/** Whether `key` resolves in EN: to a string, or to a plural family. */
function resolves(ns: Json, key: string): boolean {
  if (typeof lookup(ns, key) === 'string') return true;
  const cut = key.lastIndexOf('.');
  const parent = cut < 0 ? ns : lookup(ns, key.slice(0, cut));
  const leaf = key.slice(cut + 1);
  return !!parent && typeof parent === 'object' && `${leaf}_other` in (parent as Json);
}

/**
 * The `ns:key` a literal key reads in English, or null when it reads nothing. A key without a
 * namespace is looked up in the file's namespaces, or in all of them when the file asked for none.
 */
export function resolveKey(
  key: string,
  fileNs: string[] | null,
  locales: Record<string, { en: Json }>,
): string | null {
  const colon = key.indexOf(':');
  if (colon > 0) {
    const ns = key.slice(0, colon);
    return locales[ns] && resolves(locales[ns].en, key.slice(colon + 1)) ? key : null;
  }
  const pool = fileNs ?? Object.keys(locales);
  const hit = pool.find((n) => !!locales[n] && resolves(locales[n].en, key));
  return hit === undefined ? null : `${hit}:${key}`;
}
