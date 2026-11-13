// SPDX-License-Identifier: AGPL-3.0-only
// ADR-256: a table whose rows are ticked for a bulk action gets its checkbox column and its
// "select all" from `DataTable`'s `selection`, and from nowhere else. Before this, five tables had
// four different ways to select everything and one had none.
//
// What these checks cannot see is said beside each. A hand-built table (Discovery's candidates)
// and a list of boxes (the credential picker, the folder scopes, Rediscover - Inc.2) use
// `SelectAllBox` directly; nothing here forces that, the rule in `ui-conventions.md` does.

import { describe, expect, it } from 'vitest';
import { SRC, codeOnly, readSources } from './testSupport/sources';
import { flattenStrings, loadLocales } from './testSupport/locales';

/** Files that draw a `DataTable` and also hold a checkbox that is not a row's selection. */
const CHECKBOX_BESIDE_A_TABLE: Record<string, string> = {
  'pages/ApiTokensPage.tsx': "the token dialog's folder-scope list, not a table column",
  'pages/UsersPage.tsx': "the user dialog's folder-scope list, not a table column",
};


describe('a table selects its rows through DataTable (ADR-256)', () => {
  const sources = readSources(SRC);
  const tables = sources.filter(([p, text]) => p.endsWith('.tsx') && /<DataTable\b/.test(codeOnly(text)));

  it('reads the tables it is about', () => {
    // Floor: the files that draw a DataTable were found, the selecting ones among them.
    expect(tables.length).toBeGreaterThanOrEqual(30);
    const selecting = tables.filter(([, text]) => /\bselection=\{/.test(text)).map(([p]) => p);
    expect(selecting.length).toBeGreaterThanOrEqual(4);
  });

  it('no file that draws a DataTable writes a checkbox of its own', () => {
    const offenders = tables
      .filter(([, text]) => /type="checkbox"/.test(codeOnly(text)))
      .map(([p]) => p)
      .sort();
    expect(offenders).toEqual(Object.keys(CHECKBOX_BESIDE_A_TABLE).sort());
  });

  it('no screen has a "select all" or "clear selection" string of its own', () => {
    // The one pair is `common:select.*`. Checked in EN, which every key has.
    const namespaces = Object.entries(loadLocales());
    expect(namespaces.length).toBeGreaterThan(10);
    const offenders = namespaces.flatMap(([ns, { en }]) =>
      Object.keys(flattenStrings(en))
        .filter((k) => /(^|\.)(selectAll|pickAll|clearSelection|unpick)$/.test(k))
        .filter(() => ns !== 'common')
        .map((k) => `${ns}: ${k}`),
    );
    expect(offenders).toEqual([]);
  });
});
