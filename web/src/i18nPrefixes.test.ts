// SPDX-License-Identifier: AGPL-3.0-only
// ADR-150 決定 4(a): a runtime-built `t()` key whose PREFIX names nothing renders raw, in both
// locales, and nothing else can see it.
//
// `i18nEnumKeys.test.ts` next door walks each enum and demands a string per member — it proves the
// *members* are covered. It cannot prove the call site spells the prefix the way the locale does:
// v0.2.17 shipped `t(`audit.status.${s}`)` while the strings lived under `audit.statusClass.`, so
// the Audit log's Status filter offered three raw keys. That passed EN⟷JA parity (both locales
// were equally missing it), passed `i18nEnumKeys` (which pins the locale side, not the caller),
// passed `tsc` (`t()` takes a `string`, deliberately — `i18n-mechanism`), and passed the Tier1
// walk (a raw key is still text on the screen).
//
// So this reads every `t(`…${` in `src/` as text and asks the locales one question per prefix:
// does the object the prefix points at exist, and — when the prefix ends mid-key — does at least
// one key under it start with that stem? Both locales are asked, in the namespace the call site
// names or, when it names none, in any namespace (a component's default namespace is set by
// `useTranslation('…')` far from the call, and resolving that would be a second i18next).
//
// Loose on purpose: it says a prefix *exists*, not that every member under it does — that is the
// enum test's job. What it catches is the spelling class, which is the one that had no gate.
import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';

const SRC = __dirname;
const LOCALES = join(SRC, 'locales');

type Json = Record<string, unknown>;

/** Every `.ts`/`.tsx` under `src/` that is production code: not a test, not the generated `api/`. */
function sourceFiles(dir: string, out: string[] = []): string[] {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) {
      if (e.name !== 'api') sourceFiles(p, out);
    } else if (/\.tsx?$/.test(e.name) && !e.name.endsWith('.test.ts')) {
      out.push(p);
    }
  }
  return out;
}

const rel = (p: string) => relative(SRC, p).split('\\').join('/');

/** Every namespace, loaded from disk so a new locale file is covered without editing this test. */
function loadLocales(): Record<string, { en: Json; ja: Json }> {
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
    if (lineText.startsWith('//') || lineText.startsWith('*') || lineText.startsWith('/*')) continue;
    const line = before.split('\n').length;
    out.push({ file, line, prefix: m[1] });
  }
  return out;
}

/** Resolve a dotted key path against a namespace object; undefined when any hop is missing. */
function lookup(ns: Json, path: string): unknown {
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
function existsIn(ns: Json, rest: string): boolean {
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

describe('every runtime-built t() prefix names something in both locales', () => {
  const locales = loadLocales();
  const files = sourceFiles(SRC);
  const sites = files.flatMap((f) => prefixesIn(readFileSync(f, 'utf8'), rel(f)));

  it('inspected the tree it is supposed to be reading', () => {
    // A wrong path, or a regex that stopped matching, would make the assertion below vacuously
    // true — the one failure a guard cannot have, because it looks exactly like success. The
    // floors are well under the measured numbers (222 sites / 134 prefixes at v0.3.21) so the
    // count can fall as keys move to registries without the test going red for the wrong reason.
    expect(files.length).toBeGreaterThan(100);
    expect(sites.length).toBeGreaterThan(100);
    expect(new Set(sites.map((s) => s.prefix)).size).toBeGreaterThan(50);
    expect(Object.keys(locales).length).toBeGreaterThan(20);
  });

  it('recognises a call site, resolves a real prefix, and refuses an invented one', () => {
    // The detector and the resolver, each proven on one positive and one negative — a test that
    // only ever sees the tree pass cannot tell "nothing wrong" from "looked at nothing".
    const found = prefixesIn(
      [
        'const s = t(`nope.zilch.${k}`);',
        'const u = i18n.t(`format:state.${x}`);',
        ' * rather than one built from the enum (`` t(`shell.kind.${kind}`) ``):',
        '// t(`also.prose.${k}`)',
      ].join('\n'),
      'x.ts',
    );
    expect(found).toEqual([
      { file: 'x.ts', line: 1, prefix: 'nope.zilch.' },
      { file: 'x.ts', line: 2, prefix: 'format:state.' },
    ]);
    expect(prefixExists('format:state.', locales)).toBe(true);
    expect(prefixExists('state.', locales)).toBe(true); // namespace-less, found in `format`
    expect(prefixExists('nope.zilch.', locales)).toBe(false);
    // The bug this file exists for, spelled exactly as it shipped: the strings are under
    // `audit.statusClass.`, so `audit.status.` must NOT resolve — a stem match on `status` would
    // have made this green while the operator was reading raw keys.
    expect(prefixExists('audit.statusClass.', locales)).toBe(true);
    expect(prefixExists('access:audit.status.', locales)).toBe(false);
    // A stem that ends mid-key matches a key starting with it, in the namespace named.
    expect(prefixExists('nodes:interfaces.spee', locales)).toBe(true);
  });

  it('finds no call site whose prefix is missing from a locale', () => {
    const missing = sites
      .filter((s) => !prefixExists(s.prefix, locales))
      .map((s) => `${s.file}:${s.line} t(\`${s.prefix}\${…\`)`);
    expect(missing).toEqual([]);
  });
});
