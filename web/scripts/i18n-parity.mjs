// EN/JA translation parity check (run via `npm run i18n:check`).
//
// English is the canonical key set. For every namespace it verifies:
//   - Japanese has no key that English lacks (a typo or stale key) → error.
//   - Japanese is not missing any English key → error, EXCEPT English-only plural forms
//     (`*_one`), which Japanese legitimately omits (it has no singular/plural distinction).
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
  if (!extra.length && !missing.length) console.log(`✓ ${ns}`);
}

if (errors) {
  console.error(`\ni18n parity FAILED with ${errors} issue(s).`);
  process.exit(1);
}
console.log('\ni18n parity OK.');
