// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import en from '../../locales/en/system.json';
import ja from '../../locales/ja/system.json';
import type { MerakiImported } from '../../types/api';
import { merakiImportMessage } from './merakiImportResult';

const result = (
  imported: number,
  filed: Partial<MerakiImported['filed']> = {},
  ranges_configured = true,
  skipped: { waiting_lan?: number; bound_elsewhere?: number } = {},
): MerakiImported => ({
  imported,
  ranges_configured,
  filed: { matched: 0, ambiguous: 0, unmatched: 0, no_address: 0, ...filed },
  waiting_lan: skipped.waiting_lan ?? 0,
  bound_elsewhere: skipped.bound_elsewhere ?? 0,
});

const keys = (r: MerakiImported) => merakiImportMessage(r, 'Acme').map((p) => p.key);

describe('merakiImportMessage', () => {
  it('says why an MX waiting for its LAN read was not imported, never that it was monitored already', () => {
    expect(merakiImportMessage(result(0, {}, true, { waiting_lan: 2 }), 'Acme')).toEqual([
      { key: 'meraki.import.waitingLan', args: { count: 2 } },
    ]);
    expect(keys(result(1, { unmatched: 1 }, true, { waiting_lan: 1, bound_elsewhere: 1 }))).toEqual([
      'meraki.import.done',
      'meraki.import.filed.none',
      'meraki.import.waitingLan',
      'meraki.import.boundElsewhere',
    ]);
  });

  it('says nothing was imported, and nothing about filing, when every device was skipped', () => {
    expect(merakiImportMessage(result(0), 'Acme')).toEqual([
      { key: 'meraki.import.doneNone', args: {} },
    ]);
  });

  it('names the organization folder when no device matched a range', () => {
    expect(merakiImportMessage(result(3, { unmatched: 2, no_address: 1 }), 'Acme')).toEqual([
      { key: 'meraki.import.done', args: { count: 3 } },
      { key: 'meraki.import.filed.none', args: { folder: 'Acme' } },
    ]);
  });

  it('reads the same when filing was switched off and all four counts are zero', () => {
    expect(keys(result(3))).toEqual(['meraki.import.done', 'meraki.import.filed.none']);
    expect(keys(result(3, {}, false))).toEqual(['meraki.import.done', 'meraki.import.filed.none']);
  });

  it('does not name the organization folder when every device was filed by range', () => {
    expect(merakiImportMessage(result(2, { matched: 2 }), 'Acme')).toEqual([
      { key: 'meraki.import.done', args: { count: 2 } },
      { key: 'meraki.import.filed.all', args: {} },
    ]);
  });

  it('splits the count when only some matched, and the two halves add up', () => {
    const parts = merakiImportMessage(result(5, { matched: 2, unmatched: 2, no_address: 1 }), 'Acme');
    expect(parts[1]).toEqual({
      key: 'meraki.import.filed.some',
      args: { matched: 2, rest: 3, folder: 'Acme' },
    });
  });

  it('says an ambiguous match in its own sentence, and only when there is one', () => {
    expect(keys(result(4, { matched: 1, ambiguous: 2, unmatched: 1 }))).toEqual([
      'meraki.import.done',
      'meraki.import.filed.some',
      'meraki.import.filed.ambiguous',
    ]);
    expect(keys(result(4, { matched: 1, unmatched: 3 }))).not.toContain(
      'meraki.import.filed.ambiguous',
    );
  });

  // A key built here and missing from BOTH locales is "in parity" and renders as its raw self.
  it('only ever names keys that exist in both locales', () => {
    const lookup = (root: unknown, key: string): unknown =>
      key.split('.').reduce<unknown>((o, k) => (o as Record<string, unknown> | undefined)?.[k], root);
    const every = [
      result(0),
      result(3, { unmatched: 3 }),
      result(2, { matched: 2 }),
      result(5, { matched: 2, ambiguous: 1, unmatched: 2 }),
    ].flatMap((r) => merakiImportMessage(r, 'Acme'));
    expect(every.length).toBeGreaterThan(6);
    for (const part of every) {
      for (const [name, locale] of [
        ['en', en],
        ['ja', ja],
      ] as const) {
        // A counted sentence is stored under its plural forms; JA has `_other` only.
        const found =
          lookup(locale, part.key) ?? lookup(locale, `${part.key}_other`);
        expect(typeof found, `${name}: ${part.key}`).toBe('string');
      }
    }
  });
});
