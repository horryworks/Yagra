// EN/JA translation parity check (run via `npm run i18n:check`).
//
// English is the canonical key set. For every namespace it verifies:
//   - Japanese has no key that English lacks (a typo or stale key) → error.
//   - Japanese is not missing any English key → error, EXCEPT English-only plural forms
//     (`*_one`), which Japanese legitimately omits (it has no singular/plural distinction).
//   - A translated value carries the same `{{placeholder}}` tokens and the same `<tag>`s as its
//     English original — same names, same counts. Position is free: Japanese word order moves a
//     tag or a slot around, and that is correct, so only the multiset is compared.
//
// That last check exists because nothing else could see it. A JA value that drops `{{count}}`
// renders the literal text with the number silently gone, and one that drops `<lnk>` makes a
// `<Trans>` link vanish — neither is a key-set difference, a type error, or a runtime throw. The
// 2026-08 natural-Japanese rewrite touched every value in all 22 namespaces with no mechanical
// cover for this at all; the gate was added rather than trusting a diff read.
//
// i18next's runtime `fallbackLng: 'en'` means a missing JA key would silently render English, so
// this check is what actually keeps Japanese complete. The vitest suite runs the same logic
// (src/i18n.test.ts) so CI gates on it too; this CLI is for quick local runs.

import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const LOCALES = join(dirname(fileURLToPath(import.meta.url)), '..', 'src', 'locales');

/** Flatten a nested object to dot-path leaf keys. */
function flatten(obj, prefix = '', out = new Set()) {
  for (const [k, v] of Object.entries(obj)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === 'object' && !Array.isArray(v)) flatten(v, key, out);
    else out.add(key);
  }
  return out;
}

/** Flatten to a dot-path → string map (the values, which `flatten` deliberately drops). */
function flattenValues(obj, prefix = '', out = new Map()) {
  for (const [k, v] of Object.entries(obj)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === 'object' && !Array.isArray(v)) flattenValues(v, key, out);
    else if (typeof v === 'string') out.set(key, v);
  }
  return out;
}

/** Multiset of the interpolation tokens and pseudo-HTML tags in one string. */
function tokens(s) {
  const bag = new Map();
  const add = (t) => bag.set(t, (bag.get(t) ?? 0) + 1);
  for (const m of s.match(/\{\{[^}]+\}\}/g) ?? []) add(m.trim());
  for (const m of s.match(/<\/?[A-Za-z][A-Za-z0-9]*>/g) ?? []) add(`<${m.replace(/[</>]/g, '')}>`);
  return bag;
}

/** Human-readable diff of two token multisets, or '' when they agree. */
function tokenDiff(en, ja) {
  const [a, b] = [tokens(en), tokens(ja)];
  const parts = [];
  for (const t of new Set([...a.keys(), ...b.keys()])) {
    const [n, m] = [a.get(t) ?? 0, b.get(t) ?? 0];
    if (n !== m) parts.push(`${t} ×${n} in en, ×${m} in ja`);
  }
  return parts.join('; ');
}

const load = (lng, ns) => JSON.parse(readFileSync(join(LOCALES, lng, ns), 'utf8'));

let errors = 0;
const namespaces = readdirSync(join(LOCALES, 'en')).filter((f) => f.endsWith('.json'));

for (const ns of namespaces) {
  const en = flatten(load('en', ns));
  let ja;
  try {
    ja = flatten(load('ja', ns));
  } catch {
    console.error(`✗ ${ns}: missing ja/${ns}`);
    errors += 1;
    continue;
  }
  const extra = [...ja].filter((k) => !en.has(k));
  const missing = [...en].filter((k) => !ja.has(k) && !k.endsWith('_one'));
  if (extra.length) {
    console.error(`✗ ${ns}: ja has keys not in en: ${extra.join(', ')}`);
    errors += extra.length;
  }
  if (missing.length) {
    console.error(`✗ ${ns}: ja is missing: ${missing.join(', ')}`);
    errors += missing.length;
  }

  // Values: same placeholders and tags, in any position.
  const enVals = flattenValues(load('en', ns));
  const jaVals = flattenValues(load('ja', ns));
  const drifted = [];
  for (const [key, jaText] of jaVals) {
    const enText = enVals.get(key);
    if (enText === undefined) continue; // already reported as an extra key
    const diff = tokenDiff(enText, jaText);
    if (diff) drifted.push(`${key} (${diff})`);
  }
  if (drifted.length) {
    console.error(`✗ ${ns}: ja placeholders/tags differ from en: ${drifted.join(', ')}`);
    errors += drifted.length;
  }

  if (!extra.length && !missing.length && !drifted.length) console.log(`✓ ${ns}`);
}

if (errors) {
  console.error(`\ni18n parity FAILED with ${errors} issue(s).`);
  process.exit(1);
}
console.log('\ni18n parity OK.');
