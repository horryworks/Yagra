#!/usr/bin/env node
// SPDX-License-Identifier: AGPL-3.0-only
// Regenerates crates/yagra-oui/data/oui.tsv from the IEEE Registration Authority's public
// listings (ADR-180).
//
//   node scripts/oui-refresh.mjs           rewrite the committed table
//   node scripts/oui-refresh.mjs --check   exit 1 if the published registries differ from it
//
// The table is committed, never fetched at build time: images are built in Docker and on
// GitHub-hosted runners, and a deployment may sit on a closed network (ADR-045). So this runs by
// hand — `/docs` names it — and CI does not run it: a check that fails by date would break a
// rebuild of an old tag.
//
// Only the prefix and the organization name are kept. The postal address the CSVs also carry is
// of no use to an operator and triples the size.
//
// YAGRA_OUI_WITHHOLD may name a file of prefixes (one per line, `#` comments allowed) to leave out.
// It exists for a checkout whose own rules forbid certain organization names in the tree; the file
// lives outside the repository, and the header records only HOW MANY were withheld, never which.
// A withheld prefix then has no maker at all — the lookup refuses to fall back to the IEEE's own
// parent block. Both modes apply the same list, so --check stays meaningful.

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const OUT = join(dirname(fileURLToPath(import.meta.url)), '..', 'crates', 'yagra-oui', 'data', 'oui.tsv');

// Longest prefix first is irrelevant here — the lookup decides that — but the order of this list
// is the order the header reports the counts in.
const REGISTRIES = [
  { name: 'MA-L', hex: 6, url: 'https://standards-oui.ieee.org/oui/oui.csv' },
  { name: 'MA-M', hex: 7, url: 'https://standards-oui.ieee.org/oui28/mam.csv' },
  { name: 'MA-S', hex: 9, url: 'https://standards-oui.ieee.org/oui36/oui36.csv' },
];

// IEEE's server refuses some default client strings.
const USER_AGENT = 'yagra-oui-refresh (+https://github.com/horryworks/Yagra)';

/** RFC 4180 fields: quoted fields may hold commas, doubled quotes and line breaks. */
function parseCsv(text) {
  const rows = [];
  let row = [];
  let field = '';
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quoted) {
      if (c === '"') {
        if (text[i + 1] === '"') {
          field += '"';
          i++;
        } else {
          quoted = false;
        }
      } else {
        field += c;
      }
    } else if (c === '"') {
      quoted = true;
    } else if (c === ',') {
      row.push(field);
      field = '';
    } else if (c === '\n' || c === '\r') {
      if (c === '\r' && text[i + 1] === '\n') i++;
      row.push(field);
      rows.push(row);
      row = [];
      field = '';
    } else {
      field += c;
    }
  }
  if (field !== '' || row.length > 0) {
    row.push(field);
    rows.push(row);
  }
  return rows;
}

/** One line of text: no tab, no line break, no run of spaces. */
function clean(s) {
  return s.replace(/[\t\r\n]+/g, ' ').replace(/ {2,}/g, ' ').trim();
}

async function fetchRegistry({ name, hex, url }) {
  const res = await fetch(url, { headers: { 'User-Agent': USER_AGENT } });
  if (!res.ok) throw new Error(`${name}: HTTP ${res.status} from ${url}`);
  const rows = parseCsv(await res.text());
  const header = rows.shift();
  if (header?.[0] !== 'Registry' || header?.[1] !== 'Assignment' || header?.[2] !== 'Organization Name') {
    throw new Error(`${name}: unexpected header ${JSON.stringify(header)}`);
  }
  const out = [];
  // The published file repeats a few prefixes (an old and a new name for one assignment). The
  // first line wins, and the header says how many were dropped so a jump is visible in review.
  const seen = new Set();
  let duplicates = 0;
  for (const r of rows) {
    if (r.length < 3 || r[0] === '') continue;
    if (r[0] !== name) throw new Error(`${name}: row names registry ${r[0]}`);
    const prefix = r[1].trim().toUpperCase();
    if (!new RegExp(`^[0-9A-F]{${hex}}$`).test(prefix)) throw new Error(`${name}: bad prefix ${r[1]}`);
    const org = clean(r[2]);
    // "Private" is a withheld name, not a maker; leaving it out lets a shorter parent answer
    // (or nothing), which reads better than a vendor called "Private".
    if (org === '' || org === 'Private') continue;
    if (seen.has(prefix)) {
      duplicates++;
      continue;
    }
    seen.add(prefix);
    out.push([prefix, org]);
  }
  return { rows: out, duplicates };
}

function readWithheld() {
  const path = process.env.YAGRA_OUI_WITHHOLD;
  if (!path) return new Set();
  return new Set(
    readFileSync(path, 'utf8')
      .split(/\r?\n/)
      .map((l) => l.replace(/#.*/, '').trim().toUpperCase())
      .filter((l) => l !== ''),
  );
}

async function build() {
  const withheld = readWithheld();
  const counts = [];
  const all = [];
  let withheldCount = 0;
  for (const reg of REGISTRIES) {
    const fetched = await fetchRegistry(reg);
    const rows = fetched.rows.filter(([p]) => !withheld.has(p));
    withheldCount += fetched.rows.length - rows.length;
    counts.push(`${reg.name}=${rows.length}`);
    if (fetched.duplicates > 0) counts.push(`${reg.name}-repeats-dropped=${fetched.duplicates}`);
    all.push(...rows);
  }
  if (withheldCount > 0) counts.push(`withheld=${withheldCount}`);
  // Sort by prefix, then by length, so the file diffs line by line between refreshes.
  all.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  const seen = new Set();
  for (const [p] of all) {
    if (seen.has(p)) throw new Error(`duplicate prefix ${p}`);
    seen.add(p);
  }
  const body = all.map(([p, o]) => `${p}\t${o}`).join('\n') + '\n';
  return { counts, body };
}

function headerFor(counts, fetched) {
  return [
    '# IEEE Registration Authority public listings (MA-L, MA-M, MA-S): prefix<TAB>organization.',
    '# Generated by scripts/oui-refresh.mjs — do not edit by hand. Sources:',
    ...REGISTRIES.map((r) => `#   ${r.url}`),
    `# fetched: ${fetched}`,
    `# counts: ${counts.join(' ')}`,
    '',
  ].join('\n');
}

const check = process.argv.includes('--check');
const { counts, body } = await build();
if (check) {
  const current = readFileSync(OUT, 'utf8');
  const currentBody = current
    .split('\n')
    .filter((l) => !l.startsWith('#'))
    .join('\n');
  if (currentBody === body) {
    console.log(`oui.tsv is current (${counts.join(' ')})`);
  } else {
    console.error(`oui.tsv differs from the published registries (${counts.join(' ')}); run without --check`);
    process.exit(1);
  }
} else {
  const fetched = new Date().toISOString().slice(0, 10);
  writeFileSync(OUT, headerFor(counts, fetched) + body);
  console.log(`wrote ${OUT} (${counts.join(' ')})`);
}
