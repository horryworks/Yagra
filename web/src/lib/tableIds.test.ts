// SPDX-License-Identifier: AGPL-3.0-only
// The registry in `tableIds.ts` against the call sites that use it (ADR-129).
//
// WHY THIS READS SOURCE AS TEXT. The call sites are `.tsx` files, which Vitest never executes
// (`vite.config.ts` sets `include: ['src/**/*.test.ts']`), so there is no way to import them and
// ask. The backend solves the same problem the same way — `repo/guards.rs` reads its module's text
// and checks each statement against a declared table list — and the two failures it protects
// against here are both silent at runtime: a table with no id remembers nothing, and two tables
// sharing an id apply one screen's widths to another.
//
// 🚨 **The floors are the load-bearing half.** This check's healthy answer is "found nothing
// wrong", so it has to be able to tell that apart from "looked at nothing". A regex that stops
// matching — because someone renames the component, or wraps it — would otherwise report a clean
// tree forever. The floors count what was *inspected*, not what was walked
// (`floor-must-count-what-was-checked`).
import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';
import { TABLE_IDS } from './tableIds';

const SRC = join(__dirname, '..');

function filesUnder(dir: string, ext: string, out: string[] = []): string[] {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) filesUnder(p, ext, out);
    else if (e.name.endsWith(ext)) out.push(p);
  }
  return out;
}

const rel = (p: string) => relative(SRC, p).split('\\').join('/');

const TSX = filesUnder(SRC, '.tsx').filter((f) => !f.includes('.test.'));

/** Every `<DataTable` opening tag in the tree, with the id it declares (or `null`). */
function callSites(): { where: string; id: string | null }[] {
  const out: { where: string; id: string | null }[] = [];
  for (const file of TSX) {
    const src = readFileSync(file, 'utf8');
    // Scan from each `<DataTable` to the end of its opening tag. Attributes are one per line in
    // this codebase, but the search is bounded by the tag rather than by a line count so a
    // reformat cannot quietly shrink what is inspected.
    let i = src.indexOf('<DataTable');
    while (i !== -1) {
      const close = src.indexOf('>', i);
      const tag = src.slice(i, close === -1 ? src.length : close);
      const id = /\btableId=(?:"([^"]+)"|\{'([^']+)'\})/.exec(tag);
      const line = src.slice(0, i).split('\n').length;
      out.push({ where: `${rel(file)}:${line}`, id: id ? (id[1] ?? id[2]) : null });
      i = src.indexOf('<DataTable', i + 1);
    }
  }
  return out;
}

/**
 * Every id a surface claims, in either of the two spellings there are.
 *
 * ⚠️ **Two, because there are two kinds of table.** A `DataTable` is handed its id as a prop; the
 * node-detail Interfaces list is not a `DataTable` (its rows are `<button>`s driving the dock
 * below) and calls `useColumnWidths('…')` itself. A check that knew only the first spelling would
 * report the Interfaces id as unused and — worse — would not notice a second surface stealing it.
 */
function declaredIds(): { where: string; id: string }[] {
  const out: { where: string; id: string }[] = [];
  const patterns = [
    /\btableId=(?:"([^"]+)"|\{'([^']+)'\})/g,
    /\buseColumnWidths\('([^']+)'\)/g,
  ];
  for (const file of TSX) {
    const src = readFileSync(file, 'utf8');
    for (const re of patterns) {
      for (const m of src.matchAll(re)) {
        const line = src.slice(0, m.index ?? 0).split('\n').length;
        out.push({ where: `${rel(file)}:${line}`, id: (m[1] ?? m[2]) as string });
      }
    }
  }
  return out;
}

describe('the table-id registry', () => {
  it('names every table exactly once', () => {
    const seen = new Set<string>();
    const dupes = TABLE_IDS.filter((id) => (seen.has(id) ? true : (seen.add(id), false)));
    expect(dupes).toEqual([]);
  });

  it('spells every id the same way', () => {
    // `<group>.<camelCaseName>`. Not taste: the id is a storage key, and a document written with
    // `events_log` beside `events.log` is two tables as far as every reader is concerned.
    for (const id of TABLE_IDS) expect(id).toMatch(/^[a-z]+\.[a-zA-Z]+$/);
  });
});

describe('the call sites', () => {
  it('inspected the tables this product actually has', () => {
    // The floor, not a statistic. 29 `<DataTable` instances were measured when this shipped; a
    // detector that silently stopped matching would otherwise pass with zero.
    expect(TSX.length).toBeGreaterThanOrEqual(150);
    expect(callSites().length).toBeGreaterThanOrEqual(29);
  });

  it('gives every table a name', () => {
    const nameless = callSites()
      .filter((c) => c.id === null)
      .map((c) => `${c.where} — <DataTable with no tableId, so it remembers nothing`);
    expect(nameless.sort()).toEqual([]);
  });

  it('uses only names the registry declares', () => {
    const known = new Set<string>(TABLE_IDS);
    const unknown = declaredIds()
      .filter((d) => !known.has(d.id))
      .map((d) => `${d.where} — "${d.id}" is not in TABLE_IDS`);
    expect(unknown.sort()).toEqual([]);
  });

  it('never uses one name for two tables', () => {
    // The failure this exists for: two screens sharing an id silently apply one screen's widths to
    // the other, and both look like they are working.
    const byId = new Map<string, string[]>();
    for (const d of declaredIds()) byId.set(d.id, [...(byId.get(d.id) ?? []), d.where]);
    const shared = [...byId.entries()]
      .filter(([, wheres]) => wheres.length > 1)
      .map(([id, wheres]) => `"${id}" is used at ${wheres.join(' and ')}`);
    expect(shared.sort()).toEqual([]);
  });

  it('declares no name nothing uses', () => {
    // A one-directional check the other way: an id left behind by a deleted screen costs nothing at
    // runtime, but it is the first thing that makes the registry stop describing the product.
    const used = new Set(declaredIds().map((d) => d.id));
    expect(TABLE_IDS.filter((id) => !used.has(id))).toEqual([]);
  });
});
