// i18n smoke + EN/JA parity tests. Covers three things:
//   1. The global instance resolves keys and interpolates (the mechanism format.ts relies on).
//   2. Switching language actually swaps strings (lazy JA load works in the node env too).
//   3. Every namespace's JA mirrors EN's keys (same rules as scripts/i18n-parity.mjs) so CI —
//      which runs `npm run test` — gates on translation completeness, not just the CLI.

import { afterAll, describe, expect, it } from 'vitest';
import i18n from './i18n';

// English namespaces are bundled; import them directly for the parity check (type-safe, no fs).
import enCommon from './locales/en/common.json';
import jaCommon from './locales/ja/common.json';
import enNav from './locales/en/nav.json';
import jaNav from './locales/ja/nav.json';
import enFormat from './locales/en/format.json';
import jaFormat from './locales/ja/format.json';
import enSettings from './locales/en/settings.json';
import jaSettings from './locales/ja/settings.json';
import enAuth from './locales/en/auth.json';
import jaAuth from './locales/ja/auth.json';

type Json = Record<string, unknown>;

function flatten(obj: Json, prefix = '', out = new Set<string>()): Set<string> {
  for (const [k, v] of Object.entries(obj)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === 'object' && !Array.isArray(v)) flatten(v as Json, key, out);
    else out.add(key);
  }
  return out;
}

const NAMESPACES: Record<string, { en: Json; ja: Json }> = {
  common: { en: enCommon, ja: jaCommon },
  nav: { en: enNav, ja: jaNav },
  format: { en: enFormat, ja: jaFormat },
  settings: { en: enSettings, ja: jaSettings },
  auth: { en: enAuth, ja: jaAuth },
};

describe('i18n mechanism', () => {
  afterAll(async () => {
    await i18n.changeLanguage('en'); // leave the shared instance back on the default
  });

  it('resolves keys and interpolates on the global instance (English)', async () => {
    await i18n.changeLanguage('en');
    expect(i18n.t('nav:sections.settings')).toBe('Settings');
    expect(i18n.t('format:state.critical')).toBe('Critical');
    expect(i18n.t('format:relative.min', { count: 5 })).toBe('5m ago');
    expect(i18n.t('settings:prefs.language')).toBe('Language');
  });

  it('swaps strings immediately when the language changes (lazy JA load)', async () => {
    await i18n.changeLanguage('ja');
    expect(i18n.t('nav:sections.settings')).toBe('設定');
    expect(i18n.t('format:state.critical')).toBe('重大');
    expect(i18n.t('format:relative.min', { count: 5 })).toBe('5 分前');
    // A missing JA key falls back to English rather than rendering blank.
    expect(i18n.t('common:actions.save')).toBe('保存');
  });
});

describe('i18n EN/JA parity', () => {
  for (const [ns, { en, ja }] of Object.entries(NAMESPACES)) {
    it(`${ns}: JA mirrors EN keys (English-only *_one plurals excepted)`, () => {
      const enKeys = flatten(en);
      const jaKeys = flatten(ja);
      const extra = [...jaKeys].filter((k) => !enKeys.has(k));
      const missing = [...enKeys].filter((k) => !jaKeys.has(k) && !k.endsWith('_one'));
      expect({ ns, extra, missing }).toEqual({ ns, extra: [], missing: [] });
    });
  }
});
