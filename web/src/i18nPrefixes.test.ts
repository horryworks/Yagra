// SPDX-License-Identifier: AGPL-3.0-only
// ADR-150 decision 4(a): a runtime-built `t()` key whose PREFIX names nothing renders raw, in both
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
import { readFileSync } from 'node:fs';
import { relative } from 'node:path';
import { sourceFiles as walkSources } from './testSupport/sources';
import { loadLocales, prefixesIn, prefixExists } from './testSupport/locales';

const SRC = __dirname;

/** Every `.ts`/`.tsx` under `src/` that is production code: not a test, not the generated `api/`. */
function sourceFiles(dir: string): string[] {
  return walkSources(dir, { skipDirs: ['api'], declarations: true });
}

const rel = (p: string) => relative(SRC, p).split('\\').join('/');

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
