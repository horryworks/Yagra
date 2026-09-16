// SPDX-License-Identifier: AGPL-3.0-only
// ADR-153 決定 8: a list's filter and sort live in the URL, and component state is not where they go.
//
// Every screen that lost its filters on a reload had the same line in it — `useState<FilterState>`
// (or the untyped `useState(() => defaultFilters(cols))`), or `useState<SortState>` for a sort. There
// were thirty-odd of them across tabs, pages and report bodies, and each had a comment explaining
// why *that* one was fine: "a glance, not a view someone sends", "two tables share this route",
// "embedded twice on one route". The two real reasons (a shared route, a colliding key) now have an
// answer in the codec — a per-table prefix — and the route ledger in `lib/filterSpecRegistry.test.ts`
// checks the keys stay disjoint. So there is no exemption list here, and a new one is a decision to
// write down in the ADR first, not a line to add below.
//
// ⚠️ **What this cannot see**, so nobody reads it as more than it is:
//   - a search box held as `useState('')` (the MIB repository's was one). A plain string state is
//     far too common to ban, so a new free-text search has to reach for `useUrlTerm` by review;
//   - a chip held as `useState<'all' | …>` outside the report bodies, for the same reason;
//   - state kept in a store instead of `useState`.
// The hooks that ARE the sanctioned homes (`useFilterParams`, `useClientFilters`, `useUrlTerm`,
// `useEnumParam`, `useSortParams`) hold no `useState` of either type, so they need no carve-out.

import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';

const SRC = join(__dirname);

function sources(dir: string, out: string[] = []): string[] {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) sources(p, out);
    else if (/\.tsx?$/.test(e.name) && !/\.test\.tsx?$/.test(e.name) && !e.name.endsWith('.d.ts')) out.push(p);
  }
  return out;
}

const rel = (p: string) => relative(SRC, p).split('\\').join('/');

// Built at runtime so this file's own text holds none of the patterns it looks for — a literal here
// would be a needle that matches the line it is written on (`self-matching-needle-has-two-directions`).
const STATE = ['use', 'State'].join('');
const PATTERNS: readonly { name: string; re: RegExp }[] = [
  { name: `${STATE}<FilterState>`, re: new RegExp(`\\b${STATE}\\s*<\\s*FilterState\\s*>`) },
  { name: `${STATE}<SortState>`, re: new RegExp(`\\b${STATE}\\s*<\\s*SortState\\s*>`) },
  {
    name: `${STATE}(() => defaultFilters(…))`,
    re: new RegExp(`\\b${STATE}\\s*\\(\\s*(\\(\\)\\s*=>\\s*)?defaultFilters\\s*\\(`),
  },
];

/** Every line of `text` matching a pattern, as `line: pattern`. Comment lines are skipped: several
 *  files explain in prose what they used to hold. */
function localFilterState(text: string): string[] {
  const out: string[] = [];
  text.split(/\r?\n/).forEach((line, i) => {
    const code = line.trimStart();
    if (code.startsWith('//') || code.startsWith('*') || code.startsWith('/*')) return;
    for (const p of PATTERNS) if (p.re.test(line)) out.push(`${i + 1}: ${p.name}`);
  });
  return out;
}

describe('filter and sort state lives in the URL (ADR-153)', () => {
  const files = sources(SRC);

  it('inspected the tree it is supposed to be reading', () => {
    // A walk that read nothing would pass the check below by finding nothing
    // (`floor-must-count-what-was-checked`). The floor counts files read, which does not shrink when
    // the work succeeds.
    expect(files.length).toBeGreaterThan(400);
    expect(files.some((f) => rel(f) === 'pages/NodesPage.tsx')).toBe(true);
  });

  it('still recognises each shape it bans, and ignores one in a comment', () => {
    // The accept side: a detector that matched nothing would satisfy the real check perfectly.
    const lines = [
      `  const [filters, setFilters] = ${STATE}<FilterState>(() => defaultFilters(cols));`,
      `  const [sort, setSort] = ${STATE}<SortState>(DEFAULT_SORT);`,
      `  const [f, setF] = ${STATE}(() => defaultFilters(cols));`,
      `  // const [filters, setFilters] = ${STATE}<FilterState>({});`,
    ];
    expect(localFilterState(lines.join('\n'))).toEqual([
      `1: ${STATE}<FilterState>`,
      `2: ${STATE}<SortState>`,
      `3: ${STATE}(() => defaultFilters(…))`,
    ]);
  });

  it('finds no list filter or sort held in component state', () => {
    const offenders = files.flatMap((f) =>
      localFilterState(readFileSync(f, 'utf8')).map((hit) => `${rel(f)}:${hit}`),
    );
    expect(
      offenders,
      'hold it in the URL: useFilterParams / useClientFilters (with a prefix when the route has ' +
        'another table), useEnumParam for a chip, useSortParams for a sort — see ADR-153',
    ).toEqual([]);
  });
});
