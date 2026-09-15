// SPDX-License-Identifier: AGPL-3.0-only
// What a translated string must carry over from its English original: the same `{{placeholder}}`
// tokens and the same `<tag>`s, as a multiset. Position is free — Japanese word order moves a slot
// or a tag around, and that is correct — so only the counts are compared.
//
// This exists because nothing else can see the failure. A JA value that drops `{{count}}` renders
// the literal text with the number silently gone, and one that drops `<lnk>` makes a `<Trans>`
// link vanish. Neither is a key-set difference, a type error, or a runtime throw. It was a CLI
// (`scripts/i18n-parity.mjs`) whose header said the Vitest suite ran the same logic; the key-set
// half did, this half did not, and CI runs `npm run test`. ADR-150 決定 4(b) made the test the one
// implementation and `npm run i18n:check` an invocation of it.

export type Json = Record<string, unknown>;

/** Flatten a locale object to a dot-path → string map (only the leaves that are strings). */
export function flattenValues(
  obj: Json,
  prefix = '',
  out = new Map<string, string>(),
): Map<string, string> {
  for (const [k, v] of Object.entries(obj)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === 'object' && !Array.isArray(v)) flattenValues(v as Json, key, out);
    else if (typeof v === 'string') out.set(key, v);
  }
  return out;
}

/** Multiset of the interpolation tokens and pseudo-HTML tags in one string. */
export function tokens(s: string): Map<string, number> {
  const bag = new Map<string, number>();
  const add = (t: string) => bag.set(t, (bag.get(t) ?? 0) + 1);
  for (const m of s.match(/\{\{[^}]+\}\}/g) ?? []) add(m.trim());
  // `<lnk>` and `</lnk>` count as the same tag: what must survive translation is that the link
  // is still there, and a closing tag with no opening one is the Trans component's error to raise.
  for (const m of s.match(/<\/?[A-Za-z][A-Za-z0-9]*>/g) ?? []) add(`<${m.replace(/[</>]/g, '')}>`);
  return bag;
}

/** Human-readable diff of two token multisets, or '' when they agree. */
export function tokenDiff(en: string, ja: string): string {
  const [a, b] = [tokens(en), tokens(ja)];
  const parts: string[] = [];
  for (const t of new Set([...a.keys(), ...b.keys()])) {
    const [n, m] = [a.get(t) ?? 0, b.get(t) ?? 0];
    if (n !== m) parts.push(`${t} ×${n} in en, ×${m} in ja`);
  }
  return parts.join('; ');
}
