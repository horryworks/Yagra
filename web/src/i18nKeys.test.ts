// SPDX-License-Identifier: AGPL-3.0-only
// ADR-200 G1 and G2: every literal `t()` key names a string, and every string is asked for.
//
// G1. A key missing from BOTH locales passes EN⟷JA parity, passes `tsc` (`t()` takes a `string`),
// and passes the Tier1 walk — a raw key is still text on the screen. Three shipped that way
// (`common:status.loading`, `audit.filter.allStatuses`, `dependency.optOut`). So this reads each
// literal key in `src/` and looks it up in the namespaces its file asked for.
//
// G2. A string nothing asks for is dead weight that still gets translated, reviewed and counted by
// `proseBudget.test.ts`. "Asked for" is deliberately generous (see `isUsed`): the same spelling
// anywhere, a runtime-built prefix that covers it, or an ancestor path held in a table. It can miss
// a dead key whose spelling also means something else; it should never call a live key dead.
//
// `i18nPrefixes.test.ts` covers the runtime-built half of G1: a template key's prefix.

import { describe, expect, it } from 'vitest';
import { join } from 'node:path';
import { isCommentLine, readSources, SRC } from './testSupport/sources';
import {
  flattenStrings,
  loadLocales,
  namespacesOf,
  resolveKey,
  type Json,
} from './testSupport/locales';

const PLURAL = /_(zero|one|two|few|many|other)$/;

/**
 * Namespaces and families that no source text names key by key, each with the reason.
 * Checked both ways: an entry that stops covering any key leaves the list.
 */
const UNUSED_OK: Record<string, string> = {
  'metricMeanings:': 'generated from metric_meaning.rs; lib/metricMeaning.ts builds every key',
  'alertNames:': 'lib/alertName.ts assembles the key from the alert parts',
};

export interface KeySite {
  line: number;
  key: string;
}

/**
 * Every literal key in one source text: the first argument of `t(` / `i18n.t(` when it is a quoted
 * string or a ternary of quoted strings, and `i18nKey="…"`. Comment lines are skipped.
 */
export function keysIn(src: string): KeySite[] {
  const out: KeySite[] = [];
  const lines = src.split('\n');
  const lineOf = (i: number) => src.slice(0, i).split('\n').length;
  const push = (i: number, key: string) => {
    const line = lineOf(i);
    if (!isCommentLine(lines[line - 1])) out.push({ line, key });
  };
  const call = /\bt\(\s*/g;
  let m: RegExpExecArray | null;
  while ((m = call.exec(src))) {
    const start = m.index + m[0].length;
    const q = src[start];
    if (q === "'" || q === '"') {
      const end = src.indexOf(q, start + 1);
      if (end > start) push(m.index, src.slice(start + 1, end));
      continue;
    }
    // A ternary: read the argument to its top-level `,` or `)` and take the quoted arms.
    let depth = 0;
    let i = start;
    for (; i < src.length; i++) {
      const c = src[i];
      if (c === '(' || c === '[' || c === '{') depth++;
      else if (c === ')' || c === ']' || c === '}') {
        if (depth === 0) break;
        depth--;
      } else if (c === ',' && depth === 0) break;
    }
    const arg = src.slice(start, i);
    if (!arg.includes('?') || arg.includes('`')) continue;
    for (const a of arg.matchAll(/[?:]\s*(['"])([^'"\n]+)\1/g)) push(m.index, a[2]);
  }
  for (const a of src.matchAll(/\bi18nKey=\{?\s*(['"])([^'"]+)\1/g)) push(a.index ?? 0, a[2]);
  return out;
}

export function keyExists(
  key: string,
  fileNs: string[] | null,
  locales: Record<string, { en: Json }>,
): boolean {
  return resolveKey(key, fileNs, locales) !== null;
}

/** What the source text says about which keys are in use. */
export interface UseEvidence {
  /** Every quoted literal of key-like characters. */
  literals: Set<string>;
  /** Every runtime-built prefix: `` `PREFIX${ `` and `'PREFIX' +`. */
  prefixes: Set<string>;
}

export function evidenceFrom(texts: string[]): UseEvidence {
  const literals = new Set<string>();
  const prefixes = new Set<string>();
  for (const src of texts) {
    for (const m of src.matchAll(/(['"`])([A-Za-z0-9_:.-]+)\1/g)) literals.add(m[2]);
    for (const m of src.matchAll(/`([A-Za-z0-9_:.-]+)\$\{/g)) prefixes.add(m[1]);
    for (const m of src.matchAll(/(['"])([A-Za-z0-9_:.-]+)\1\s*\+/g)) prefixes.add(m[2]);
  }
  // A one-word prefix (`` `${a}` ``-shaped stems, `'x' +`) would cover half a namespace.
  for (const p of [...prefixes]) if (!/[.:]/.test(p)) prefixes.delete(p);
  return { literals, prefixes };
}

/** The four ways a key counts as asked for. `ns` is its namespace, `key` the path inside it. */
export function isUsed(ns: string, key: string, ev: UseEvidence): boolean {
  const base = key.replace(PLURAL, '');
  for (const k of [key, base]) {
    if (ev.literals.has(k) || ev.literals.has(`${ns}:${k}`)) return true;
  }
  for (const p of ev.prefixes) {
    const colon = p.indexOf(':');
    if (
      colon > 0
        ? p.slice(0, colon) === ns && base.startsWith(p.slice(colon + 1))
        : base.startsWith(p)
    ) {
      return true;
    }
  }
  const parts = base.split('.');
  for (let n = parts.length - 1; n >= 2; n--) {
    const anc = parts.slice(0, n).join('.');
    if (ev.literals.has(anc) || ev.literals.has(`${ns}:${anc}`)) return true;
  }
  return false;
}

const locales = loadLocales();
const sources = readSources(SRC, { skipDirs: ['api', 'locales'] });

function testTexts(): string[] {
  return readSources(join(SRC, '..', 'tests'), { includeTests: true }).map(([, s]) => s);
}

describe('G1: every literal t() key exists in English', () => {
  const sites = sources.flatMap(([file, src]) => {
    const ns = namespacesOf(src);
    return keysIn(src).map((s) => ({ file, ns, ...s }));
  });

  it('inspected the tree it is supposed to be reading', () => {
    // Measured 4,452 call sites on 2026-10-05. A floor well under that still fails a dead regex.
    expect(sources.length).toBeGreaterThan(300);
    expect(sites.length).toBeGreaterThan(3500);
  });

  it('reads the namespaces a file asked for, and every shape of literal key', () => {
    expect(
      namespacesOf("useTranslation('nodes'); useTranslation(['a', 'b']); useTranslation()"),
    ).toEqual(['nodes', 'a', 'b', 'common']);
    expect(namespacesOf('function f(t) {}')).toBeNull();
    expect(
      keysIn(
        [
          "t('a.b')",
          'i18n.t("c:d", { x })',
          "t(on ? 'e.on' : 'e.off')",
          '<Trans i18nKey="f.g" />',
          "// t('in.a.comment')",
          't(`built.${x}`)',
        ].join('\n'),
      ).map((s) => s.key),
    ).toEqual(['a.b', 'c:d', 'e.on', 'e.off', 'f.g']);
  });

  it('finds a missing key and accepts a real one', () => {
    expect(keyExists('common:status.loading', null, locales)).toBe(false);
    expect(keyExists('common:loading', null, locales)).toBe(true);
    expect(keyExists('loading', ['common'], locales)).toBe(true);
    expect(keyExists('loading', ['nodes'], locales)).toBe(false);
  });

  it('finds no literal key that is missing', () => {
    const missing = sites
      .filter((s) => !keyExists(s.key, s.ns, locales))
      .map((s) => `${s.file}:${s.line} '${s.key}'`);
    expect(missing).toEqual([]);
  });
});

describe('G2: every English string is asked for somewhere', () => {
  const ev = evidenceFrom([...sources.map(([, s]) => s), ...testTexts()]);
  const exempt = (ns: string, key: string) =>
    Object.keys(UNUSED_OK).some((e) => `${ns}:${key}`.startsWith(e));
  const all = Object.entries(locales).flatMap(([ns, { en }]) =>
    Object.keys(flattenStrings(en)).map((key) => ({ ns, key })),
  );

  it('inspected the locale files it is supposed to be reading', () => {
    // 5,648 keys on 2026-10-05.
    expect(all.length).toBeGreaterThan(4500);
    expect(ev.literals.size).toBeGreaterThan(3000);
    expect(ev.prefixes.size).toBeGreaterThan(100);
  });

  it('counts each of the four kinds of evidence, and nothing else', () => {
    const e = evidenceFrom([
      "t('x.used')",
      't(`x.fam.${k}`)',
      "const T = 'x.tbl';",
      "'x.cat.' + k",
    ]);
    expect(isUsed('n', 'x.used', e)).toBe(true);
    expect(isUsed('n', 'x.used_other', e)).toBe(true);
    expect(isUsed('n', 'x.fam.a', e)).toBe(true);
    expect(isUsed('n', 'x.tbl.title', e)).toBe(true);
    expect(isUsed('n', 'x.cat.b', e)).toBe(true);
    expect(isUsed('n', 'x.unused', e)).toBe(false);
    const q = evidenceFrom(["t('n:only.here')"]);
    expect(isUsed('n', 'only.here', q)).toBe(true);
    expect(isUsed('m', 'only.here', q)).toBe(false);
  });

  it('keeps the exemption list honest', () => {
    for (const e of Object.keys(UNUSED_OK)) {
      expect(
        all.some(({ ns, key }) => `${ns}:${key}`.startsWith(e)),
        e,
      ).toBe(true);
    }
  });

  it('finds no string that nothing asks for', () => {
    const unused = all
      .filter(({ ns, key }) => !exempt(ns, key) && !isUsed(ns, key, ev))
      .map(({ ns, key }) => `${ns}:${key}`);
    expect(unused).toEqual([]);
  });
});
