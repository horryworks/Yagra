// SPDX-License-Identifier: AGPL-3.0-only
// Lists the explanatory prose of the locale files, for the ADR-200 clean-up.
//
//   node scripts/prose-ledger.mjs system:pollers settings-tokens   # the prose under each target
//   node scripts/prose-ledger.mjs --all system:pollers              # every string, not just prose
//   node scripts/prose-ledger.mjs --diff <commit> system:pollers    # removed / changed / added
//
// A target is a namespace (`settings-tokens`) or a namespace and a key prefix (`system:pollers`).
// "Prose" is what `src/testSupport/prose.ts` means by it: five or more English words, with
// `<tag>` and `{{placeholder}}` read as spaces. Output is a Markdown table on stdout.

import { execFileSync } from 'node:child_process';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const WEB = join(dirname(fileURLToPath(import.meta.url)), '..');
const SRC = join(WEB, 'src');

const args = process.argv.slice(2);
const all = args.includes('--all');
const diffAt = args.indexOf('--diff');
const commit = diffAt >= 0 ? args[diffAt + 1] : null;
const targets = args.filter((a, i) => !a.startsWith('--') && !(diffAt >= 0 && i === diffAt + 1));
if (targets.length === 0) {
  console.error('usage: prose-ledger.mjs [--all] [--diff <commit>] <ns>[:<prefix>] ...');
  process.exit(2);
}

const wordCount = (s) =>
  s
    .replace(/<[^>]*>/g, ' ')
    .replace(/\{\{[^}]*\}\}/g, ' ')
    .split(/\s+/)
    .filter(Boolean).length;

function flatten(obj, prefix = '', out = {}) {
  for (const [k, v] of Object.entries(obj)) {
    const p = prefix ? `${prefix}.${k}` : k;
    if (typeof v === 'string') out[p] = v;
    else if (v && typeof v === 'object') flatten(v, p, out);
  }
  return out;
}

function readLocale(lng, ns, at) {
  const rel = `web/src/locales/${lng}/${ns}.json`;
  try {
    const text = at
      ? execFileSync('git', ['show', `${at}:${rel}`], { cwd: WEB, encoding: 'utf8' })
      : readFileSync(join(SRC, 'locales', lng, `${ns}.json`), 'utf8');
    return flatten(JSON.parse(text));
  } catch {
    return {};
  }
}

const jaOf = (ja, key) => ja[key] ?? ja[key.replace(/_one$/, '_other')] ?? '';

function sources() {
  const out = [];
  const walk = (d) => {
    for (const e of readdirSync(d)) {
      const p = join(d, e);
      if (statSync(p).isDirectory()) {
        if (!['api', 'locales'].includes(e)) walk(p);
      } else if (/\.tsx?$/.test(e) && !/\.test\.tsx?$/.test(e)) {
        out.push([relative(SRC, p).split(sep).join('/'), readFileSync(p, 'utf8').split('\n')]);
      }
    }
  };
  walk(SRC);
  return out;
}

let files = null;
/** Up to three `file:line` that quote the key, its `ns:key` form, or failing that its parent. */
function whereUsed(ns, key) {
  files ??= sources();
  const needles = [`'${key}'`, `"${key}"`, `'${ns}:${key}'`, `\`${key}`, `\`${ns}:${key}`];
  const parent = key.split('.').slice(0, -1).join('.');
  const hits = [];
  for (const pass of [needles, parent.includes('.') ? [`${parent}.`] : []]) {
    for (const [f, lines] of files) {
      lines.forEach((l, i) => {
        if (hits.length < 3 && pass.some((n) => l.includes(n))) hits.push(`${f}:${i + 1}`);
      });
    }
    if (hits.length) break;
  }
  return hits.join(' ');
}

const cell = (s) => String(s).replace(/\|/g, '\\|').replace(/\n/g, ' ');
const row = (cols) => console.log(`| ${cols.map(cell).join(' | ')} |`);

for (const target of targets) {
  const [ns, prefix = ''] = target.split(':');
  const inScope = (k) => prefix === '' || k === prefix || k.startsWith(`${prefix}.`);
  const en = readLocale('en', ns);
  const ja = readLocale('ja', ns);
  console.log(`\n### ${target}\n`);
  if (commit) {
    const en0 = readLocale('en', ns, commit);
    const ja0 = readLocale('ja', ns, commit);
    const keys = [...new Set([...Object.keys(en0), ...Object.keys(en)])].filter(inScope).sort();
    row(['', 'key', 'EN before', 'JA before', 'EN after', 'JA after']);
    row(['---', '---', '---', '---', '---', '---']);
    let n = 0;
    for (const k of keys) {
      const [a, b] = [en0[k], en[k]];
      if (!all && wordCount(a ?? '') < 5 && wordCount(b ?? '') < 5) continue;
      const kind = a === undefined ? 'added' : b === undefined ? 'removed' : a !== b || jaOf(ja0, k) !== jaOf(ja, k) ? 'changed' : null;
      if (!kind) continue;
      n++;
      row([kind, k, a ?? '', a === undefined ? '' : jaOf(ja0, k), b ?? '', b === undefined ? '' : jaOf(ja, k)]);
    }
    console.log(`\n${n} rows`);
    continue;
  }
  row(['key', 'words', 'EN chars', 'JA chars', 'EN', 'JA', 'used at']);
  row(['---', '---', '---', '---', '---', '---', '---']);
  let n = 0;
  let mass = 0;
  for (const [k, v] of Object.entries(en)) {
    if (!inScope(k)) continue;
    const w = wordCount(v);
    if (!all && w < 5) continue;
    n++;
    if (w >= 5) mass += v.length;
    row([k, w, v.length, jaOf(ja, k).length, v, jaOf(ja, k), whereUsed(ns, k)]);
  }
  console.log(`\n${n} rows, ${mass} EN characters of prose`);
}
