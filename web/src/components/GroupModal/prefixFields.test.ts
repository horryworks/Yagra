// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import {
  prefixDraftFrom,
  syncOwnedRows,
  prefixesChanged,
  prefixBodyFrom,
  PREFIX_PROBLEMS,
  MAX_PREFIX_ROWS,
  MAX_PREFIX_DESCRIPTION,
} from './prefixFields';
import type { NodeGroup } from '../../types/api';

const group = (prefixes: NodeGroup['prefixes']): NodeGroup =>
  ({
    id: 'g1',
    name: 'site',
    group_type: 'site',
    parent_id: null,
    sort_order: 0,
    latitude: null,
    longitude: null,
    effective_latitude: null,
    effective_longitude: null,
    geo_source: 'unset',
    geo_group: null,
    pool: null,
    prefixes,
  }) as NodeGroup;

const manual = (prefix: string, description = '') =>
  ({ prefix, description, source: 'manual' }) as NodeGroup['prefixes'][number];
const sync = (prefix: string, description = '') =>
  ({ prefix, description, source: 'sync' }) as NodeGroup['prefixes'][number];

describe('prefixDraftFrom / syncOwnedRows', () => {
  it('splits the folder rows by who owns them', () => {
    const g = group([manual('10.0.0.0/8', 'lan'), sync('172.16.0.0/12', 'netbox')]);
    expect(prefixDraftFrom(g)).toEqual([{ prefix: '10.0.0.0/8', description: 'lan' }]);
    expect(syncOwnedRows(g)).toEqual([{ prefix: '172.16.0.0/12', description: 'netbox' }]);
  });

  it('is empty for a folder with no ranges, and for no folder at all', () => {
    expect(prefixDraftFrom(group([]))).toEqual([]);
    expect(prefixDraftFrom(undefined)).toEqual([]);
    expect(syncOwnedRows(undefined)).toEqual([]);
  });
});

describe('prefixesChanged', () => {
  it('is false when nothing was touched', () => {
    const g = group([manual('10.0.0.0/8', 'lan')]);
    expect(prefixesChanged(prefixDraftFrom(g), g)).toBe(false);
  });

  it('ignores a blank row the operator added and did not fill in', () => {
    const g = group([manual('10.0.0.0/8', 'lan')]);
    expect(
      prefixesChanged([...prefixDraftFrom(g), { prefix: '  ', description: '' }], g),
    ).toBe(false);
  });

  it('sees an added, removed, or edited range', () => {
    const g = group([manual('10.0.0.0/8', 'lan')]);
    expect(prefixesChanged([{ prefix: '10.0.0.0/8', description: 'office' }], g)).toBe(true);
    expect(prefixesChanged([], g)).toBe(true);
    expect(
      prefixesChanged(
        [...prefixDraftFrom(g), { prefix: '192.168.1.0/24', description: '' }],
        g,
      ),
    ).toBe(true);
  });

  it('does not count a sync row as a removal', () => {
    const g = group([manual('10.0.0.0/8'), sync('172.16.0.0/12')]);
    expect(prefixesChanged(prefixDraftFrom(g), g)).toBe(false);
  });
});

describe('prefixBodyFrom', () => {
  // 🚨 The load-bearing property, and it reads as an omission without this reason: there is no
  // IPv6-capable range parser in the browser (`lib/cidr.ts` is IPv4-only), so a shape check here
  // would refuse every valid IPv6 range while looking helpful. PostgreSQL is the validator.
  it('does not validate the range itself — v6 passes, and so does nonsense', () => {
    const v6 = prefixBodyFrom([{ prefix: '2001:db8::/32', description: '' }], undefined);
    expect(v6).toEqual({ body: [{ prefix: '2001:db8::/32', description: '' }] });
    const junk = prefixBodyFrom([{ prefix: 'garbage', description: '' }], undefined);
    expect(junk).toEqual({ body: [{ prefix: 'garbage', description: '' }] });
  });

  it('accepts host bits untouched — the server canonicalises them', () => {
    expect(prefixBodyFrom([{ prefix: '192.168.1.5/24', description: '' }], undefined)).toEqual({
      body: [{ prefix: '192.168.1.5/24', description: '' }],
    });
  });

  it('trims, and drops a row with nothing typed in it', () => {
    expect(
      prefixBodyFrom(
        [
          { prefix: ' 10.0.0.0/8 ', description: ' lan ' },
          { prefix: '', description: '' },
        ],
        undefined,
      ),
    ).toEqual({ body: [{ prefix: '10.0.0.0/8', description: 'lan' }] });
  });

  it('treats the empty list as a valid body — that is how ranges are cleared', () => {
    expect(prefixBodyFrom([], undefined)).toEqual({ body: [] });
  });

  it('refuses a description with no range', () => {
    expect(prefixBodyFrom([{ prefix: '  ', description: 'lan' }], undefined)).toEqual({
      error: 'prefixEmpty',
    });
  });

  it('refuses a duplicate and names it', () => {
    expect(
      prefixBodyFrom(
        [
          { prefix: '10.0.0.0/8', description: '' },
          { prefix: '10.0.0.0/8', description: 'again' },
        ],
        undefined,
      ),
    ).toEqual({ error: 'prefixDuplicate', prefix: '10.0.0.0/8' });
  });

  it('refuses a range a sync owns and names it', () => {
    expect(
      prefixBodyFrom(
        [{ prefix: '172.16.0.0/12', description: '' }],
        group([sync('172.16.0.0/12')]),
      ),
    ).toEqual({ error: 'prefixOwnedBySync', prefix: '172.16.0.0/12' });
  });

  it('refuses more rows than a folder may carry', () => {
    const rows = Array.from({ length: MAX_PREFIX_ROWS + 1 }, (_, i) => ({
      prefix: `10.${i}.0.0/16`,
      description: '',
    }));
    expect(prefixBodyFrom(rows, undefined)).toEqual({ error: 'prefixTooMany' });
  });

  it('refuses a description past the cap', () => {
    expect(
      prefixBodyFrom(
        [{ prefix: '10.0.0.0/8', description: 'x'.repeat(MAX_PREFIX_DESCRIPTION + 1) }],
        undefined,
      ),
    ).toEqual({ error: 'prefixDescTooLong', prefix: '10.0.0.0/8' });
  });

  it('every declared problem is reachable', () => {
    const reached = new Set<string>();
    const cases: { draft: { prefix: string; description: string }[]; g?: NodeGroup }[] = [
      { draft: [{ prefix: '', description: 'x' }] },
      {
        draft: [
          { prefix: '10.0.0.0/8', description: '' },
          { prefix: '10.0.0.0/8', description: '' },
        ],
      },
      { draft: [{ prefix: '172.16.0.0/12', description: '' }], g: group([sync('172.16.0.0/12')]) },
      {
        draft: Array.from({ length: MAX_PREFIX_ROWS + 1 }, (_, i) => ({
          prefix: `10.${i}.0.0/16`,
          description: '',
        })),
      },
      {
        draft: [
          { prefix: '10.0.0.0/8', description: 'x'.repeat(MAX_PREFIX_DESCRIPTION + 1) },
        ],
      },
    ];
    for (const c of cases) {
      const r = prefixBodyFrom(c.draft, c.g);
      if ('error' in r) reached.add(r.error);
    }
    expect([...reached].sort()).toEqual([...PREFIX_PROBLEMS].sort());
  });
});
