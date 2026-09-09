// SPDX-License-Identifier: AGPL-3.0-only
// Generate `src/dashboard/widgetRoutes.json` from the `reads` declarations in `registry.tsx`.
//
// WHY THIS EXISTS (ADR-123 決定 6). Core derives the anonymous route allow-list from the widgets on
// the public board, so it needs the widget-type → routes table. Writing that table a second time in
// Rust would be a mirror with no guard, and this repo's own rule is that a fact written in two
// places ends up written correctly in one (`extensibility.md`). So the TypeScript registry stays
// the single source and this emits the machine-readable half. Same shape as ADR-035's OpenAPI
// chain and `mib.rs::the_committed_metric_catalog_is_current`, in the other direction.
//
// ⚠️ The output is COMMITTED and read by Rust at build time (`public_access.rs`). CI regenerates
// and fails on any diff, exactly like `schema.d.ts`.
//
// It parses `registry.tsx` as TEXT rather than importing it: the module pulls in 48 React
// components and every chart primitive, none of which node can load. The parse is deliberately
// strict — an entry it cannot read is a hard error, never a skipped row, because a generator that
// silently emits fewer widgets than exist would quietly narrow nothing and widen nothing while
// looking correct.

import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const REGISTRY = join(here, '..', 'src', 'dashboard', 'registry.tsx');
const OUT = join(here, '..', 'src', 'dashboard', 'widgetRoutes.json');

const src = readFileSync(REGISTRY, 'utf8');

// Every `type: '…'` inside the REGISTRY array, in order, each paired with the `reads: […]` that
// follows it before the next `type:`.
const entries = [];
const typeRe = /^    type: '([^']+)',$/gm;
const marks = [];
let m;
while ((m = typeRe.exec(src))) marks.push({ type: m[1], at: m.index });

for (let i = 0; i < marks.length; i++) {
  const body = src.slice(marks[i].at, i + 1 < marks.length ? marks[i + 1].at : src.length);
  const reads = /^    reads: (\[[^\]]*\]),$|^    reads: (\[[\s\S]*?\]),$/m.exec(body);
  if (!reads) throw new Error(`widget '${marks[i].type}' has no reads declaration`);
  const routes = [...(reads[1] ?? reads[2]).matchAll(/'([^']+)'/g)].map((x) => x[1]);
  if (routes.length === 0) throw new Error(`widget '${marks[i].type}' declares an empty reads list`);
  for (const r of routes) {
    if (!/^(GET|POST) \/api\/v1\//.test(r)) {
      throw new Error(`widget '${marks[i].type}' declares a malformed route: ${r}`);
    }
  }
  entries.push([marks[i].type, routes]);
}

if (entries.length < 40) {
  // A floor on what was PARSED, not on what exists: a regex that stops matching would otherwise
  // emit a small, valid-looking file and quietly close routes the board needs.
  throw new Error(`parsed only ${entries.length} widgets — the registry format probably changed`);
}

const byType = Object.fromEntries(entries.sort((a, b) => a[0].localeCompare(b[0])));
writeFileSync(OUT, JSON.stringify(byType, null, 2) + '\n');
console.log(`widgetRoutes.json: ${entries.length} widgets, ${new Set(entries.flatMap((e) => e[1])).size} distinct routes`);
