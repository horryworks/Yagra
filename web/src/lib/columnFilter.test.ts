// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the column-filter types and their derived facts (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import {
  activeFilterCount,
  activeFilterKeys,
  clearFilter,
  decodeRange,
  decodeSet,
  defaultFilters,
  encodeRange,
  encodeSet,
  filterableColumns,
  isAnyFiltered,
  readFilterParams,
  reservedKeyCollisions,
  toggleSetValue,
  writeFilterParams,
  TEXT_MODES,
  type FilterableColumn,
  type RangeShape,
} from './columnFilter';

interface Row {
  kind: string;
  message: string;
  at: number;
}

const KINDS = ['syslog', 'trap', 'webhook'];

const COLUMNS: FilterableColumn<Row>[] = [
  {
    key: 'kind',
    filter: {
      kind: 'enum',
      options: KINDS.map((v) => ({ value: v, label: v })),
      readValue: (r) => r.kind,
      allLabel: 'All kinds',
    },
  },
  {
    key: 'message',
    filter: { kind: 'text', modes: TEXT_MODES, not: true, readText: (r) => [r.message] },
  },
  {
    key: 'at',
    filter: {
      kind: 'range',
      presets: [
        { value: '1h', label: 'Last hour', seconds: 3600 },
        { value: '24h', label: 'Last 24 hours', seconds: 86400 },
        { value: 'all', label: 'All time', seconds: null },
      ],
      defaultPreset: '24h',
      readTime: (r) => r.at,
    },
  },
];

describe('defaults are derived, never written out', () => {
  it('gives every column its own "nothing set" value', () => {
    // A hand-written defaults object is the copy `filterQuery.ts::isFiltered` warns about: a filter
    // added without its clause makes the screen say "there is nothing here" while rows are hidden.
    expect(defaultFilters(COLUMNS)).toEqual({ kind: '', message: '', at: '24h' });
  });

  it('treats a NARROWING default as not-filtered, on purpose', () => {
    // Events defaults to 24h because an unbounded default made case-insensitive search unaffordable
    // (ADR-024). So "no filter" and "showing everything" are different states here, and the empty
    // state has to name the window rather than say "nothing matches these filters".
    expect(isAnyFiltered(COLUMNS, defaultFilters(COLUMNS))).toBe(false);
    expect(isAnyFiltered(COLUMNS, { kind: '', message: '', at: 'all' })).toBe(true);
  });

  it('reads a missing key as not-set rather than as a difference', () => {
    // A state object built by a screen that has not yet written every key must not read as filtered.
    expect(isAnyFiltered(COLUMNS, {})).toBe(false);
    expect(isAnyFiltered(COLUMNS, { kind: 'syslog' })).toBe(true);
  });
});

describe('activeFilterCount', () => {
  it('counts columns, not dimensions', () => {
    // The Events toolbar this replaced counted `regex` as a filter of its own, so a regex search
    // read as two. A mode is not a filter.
    expect(activeFilterCount(COLUMNS, defaultFilters(COLUMNS))).toBe(0);
    expect(activeFilterCount(COLUMNS, { kind: 'syslog,trap', message: '!~^LINK', at: '24h' })).toBe(
      2,
    );
    expect(activeFilterKeys(COLUMNS, { kind: 'syslog', message: '', at: 'all' })).toEqual([
      'kind',
      'at',
    ]);
  });
});

describe('clearFilter', () => {
  it('returns a column to its own default, not to the empty string', () => {
    const state = { kind: 'syslog', message: 'link', at: 'all' };
    expect(clearFilter(COLUMNS, state, 'at')).toEqual({
      kind: 'syslog',
      message: 'link',
      at: '24h',
    });
    expect(clearFilter(COLUMNS, state, 'kind')).toEqual({ kind: '', message: 'link', at: 'all' });
  });
});

describe('token sets', () => {
  it('joins in the spec order regardless of click order', () => {
    // A URL that changes with click order cannot be compared for equality, so a shared link differs
    // from the same view reached by clicking — and the joined string is a useEffect dependency key,
    // so a reorder re-fires the fetch.
    expect(encodeSet(['webhook', 'syslog'], KINDS)).toBe('syslog,webhook');
    expect(encodeSet(['syslog', 'webhook'], KINDS)).toBe('syslog,webhook');
  });

  it('deduplicates and drops anything the spec does not offer', () => {
    expect(encodeSet(['syslog', 'syslog'], KINDS)).toBe('syslog');
    expect(encodeSet(['syslog', 'kafka'], KINDS)).toBe('syslog');
  });

  it('decodes tolerantly', () => {
    expect(decodeSet('')).toEqual([]);
    expect(decodeSet('syslog, trap')).toEqual(['syslog', 'trap']);
    expect(decodeSet(',,syslog,,')).toEqual(['syslog']);
  });

  it('toggles without disturbing the order', () => {
    expect(toggleSetValue('', 'trap', KINDS)).toBe('trap');
    expect(toggleSetValue('trap', 'syslog', KINDS)).toBe('syslog,trap');
    expect(toggleSetValue('syslog,trap', 'syslog', KINDS)).toBe('trap');
    expect(toggleSetValue('syslog', 'syslog', KINDS)).toBe('');
  });
});

describe('reservedKeyCollisions', () => {
  it('is empty for a well-formed spec', () => {
    expect(reservedKeyCollisions(COLUMNS)).toEqual([]);
  });

  it('catches a column key that would fight the page own query params', () => {
    // The column key IS the URL key — no prefix, because the existing screens spell `severity`,
    // `state` and `q` bare and a prefix would break every bookmark taken before this shipped. This
    // function is what makes the cost of that choice checkable instead of a runtime surprise.
    const clashing = [...COLUMNS, { key: 'limit', filter: COLUMNS[0].filter }];
    expect(reservedKeyCollisions(clashing)).toEqual(['limit']);
  });

  it('catches a duplicated column key', () => {
    const dup = [...COLUMNS, { key: 'kind', filter: COLUMNS[0].filter }];
    expect(reservedKeyCollisions(dup)).toEqual(['kind']);
  });
});

describe('the URL codec', () => {
  it('round-trips through a query string', () => {
    const state = { kind: 'syslog,trap', message: '!~^LINK', at: 'all' };
    const params = new URLSearchParams();
    writeFilterParams(COLUMNS, params, state);
    expect(readFilterParams(COLUMNS, params)).toEqual(state);
  });

  it('deletes a column at its default rather than writing it', () => {
    // So a bare URL is always the default view and a `?` always means something is narrowing the
    // list — the same contract `writeEnumParam` established for the single-filter screens.
    const params = new URLSearchParams('kind=trap&at=all&message=x');
    writeFilterParams(COLUMNS, params, defaultFilters(COLUMNS));
    expect(params.toString()).toBe('');
  });

  it('leaves keys it does not own alone', () => {
    // The page's own params (`node_id` here) share the query string with the filter row.
    const params = new URLSearchParams('node_id=abc&kind=trap');
    writeFilterParams(COLUMNS, params, { ...defaultFilters(COLUMNS), kind: 'syslog' });
    expect(params.get('node_id')).toBe('abc');
    expect(params.get('kind')).toBe('syslog');
  });

  it('reads an explicitly empty value as cleared, not as absent', () => {
    // `?kind=` is what a half-applied clear looks like. For a set that is the same as unfiltered;
    // for the range it must NOT be, or an empty value would silently widen a bounded default.
    const params = new URLSearchParams('kind=&at=');
    const read = readFilterParams(COLUMNS, params);
    expect(read.kind).toBe('');
    expect(read.at).toBe('');
    expect(isAnyFiltered(COLUMNS, read)).toBe(true); // `at: ''` differs from the '24h' default
  });
});

describe('the range codec', () => {
  const spec = COLUMNS[2].filter as RangeShape;

  it('encodes a preset as itself and a custom window inside the one value', () => {
    expect(encodeRange({ preset: '1h', from: '', to: '' })).toBe('1h');
    expect(encodeRange({ preset: 'custom', from: '', to: '' })).toBe('custom');
    expect(encodeRange({ preset: 'custom', from: '2026-08-01T00:00', to: '' })).toBe(
      'custom:2026-08-01T00:00|',
    );
  });

  it('round-trips a custom window', () => {
    const v = { preset: 'custom', from: '2026-08-01T00:00', to: '2026-08-02T12:30' };
    expect(decodeRange(encodeRange(v), { ...spec, custom: true })).toEqual(v);
  });

  it('ignores a custom value on a column that does not offer one', () => {
    // Without `custom`, `custom:…` is just an unknown preset — and an unknown preset lands on the
    // default, never on "all time". Widening on a token nobody typed is how a stale bookmark turns
    // into the unbounded query a bounded default exists to prevent.
    expect(decodeRange('custom:2026-08-01T00:00|', spec)).toEqual({
      preset: spec.defaultPreset,
      from: '',
      to: '',
    });
    expect(decodeRange('fortnight', spec).preset).toBe(spec.defaultPreset);
  });

  it('reads an empty value as the default', () => {
    expect(decodeRange('', spec).preset).toBe(spec.defaultPreset);
  });
});

describe('filterableColumns', () => {
  it('keeps only the columns that opted in, in order', () => {
    const cols = [
      { key: 'a', filter: COLUMNS[0].filter },
      { key: 'b' },
      { key: 'c', filter: COLUMNS[1].filter },
    ];
    expect(filterableColumns<Row>(cols).map((c) => c.key)).toEqual([
      'a',
      'c',
    ]);
  });
});
