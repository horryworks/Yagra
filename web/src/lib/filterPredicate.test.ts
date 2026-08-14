// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the generic client-side row predicate (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import { TEXT_MODES, type FilterableColumn, type RangeFilterSpec } from './columnFilter';
import { applyFilters, buildPredicate, matchesFilters } from './filterPredicate';

interface Row {
  kind: string;
  tags: string[];
  message: string;
  at: number;
}

const NOW = Date.UTC(2026, 7, 13, 12, 0, 0);
const H = 3600_000;

const ROWS: Row[] = [
  { kind: 'syslog', tags: ['net'], message: '%LINK-3-UPDOWN: down', at: NOW - 1 * H },
  { kind: 'syslog', tags: ['sec', 'net'], message: '%%01POLICY/6/POLICYPERMIT', at: NOW - 5 * H },
  { kind: 'trap', tags: ['net'], message: 'linkDown', at: NOW - 30 * H },
  { kind: 'webhook', tags: [], message: 'deploy finished', at: NOW - 10 * H },
];

const COLUMNS: FilterableColumn<Row>[] = [
  {
    key: 'kind',
    filter: {
      kind: 'enum',
      options: ['syslog', 'trap', 'webhook'].map((v) => ({ value: v, label: v })),
      readValue: (r) => r.kind,
      allLabel: 'All kinds',
    },
  },
  {
    key: 'tags',
    filter: {
      kind: 'enum',
      options: ['net', 'sec'].map((v) => ({ value: v, label: v })),
      readValue: (r) => r.tags,
      allLabel: 'All tags',
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
        { value: '6h', label: '6h', seconds: 6 * 3600 },
        { value: '24h', label: '24h', seconds: 24 * 3600 },
        { value: 'all', label: 'All time', seconds: null },
      ],
      defaultPreset: '24h',
      readTime: (r) => r.at,
    },
  },
];

const rangeSpec = COLUMNS[3].filter as RangeFilterSpec<Row>;

const msgs = (state: Record<string, string>) =>
  applyFilters(ROWS, COLUMNS, state, NOW).map((r) => r.message);

describe('an unset filter matches everything', () => {
  it('passes every row when nothing is set but the default window', () => {
    // The 30h-old trap is outside the default 24h window — a *narrowing* default, which is exactly
    // why `isAnyFiltered` and "showing everything" are different questions.
    expect(msgs({})).toHaveLength(3);
    expect(msgs({ at: 'all' })).toHaveLength(4);
  });
});

describe('enum columns', () => {
  it('unions the selected tokens rather than intersecting them', () => {
    // Multi-select means "any of these", which is the only reading that lets a two-token selection
    // show more rows than a one-token one.
    expect(msgs({ kind: 'syslog', at: 'all' })).toHaveLength(2);
    expect(msgs({ kind: 'syslog,webhook', at: 'all' })).toHaveLength(3);
    expect(msgs({ kind: 'syslog,trap,webhook', at: 'all' })).toHaveLength(4);
  });

  it('matches a row that carries several values if any of them is selected', () => {
    expect(msgs({ tags: 'sec', at: 'all' })).toEqual(['%%01POLICY/6/POLICYPERMIT']);
    expect(msgs({ tags: 'net', at: 'all' })).toHaveLength(3);
  });

  it('excludes a row with no value once a selection exists', () => {
    // The webhook row has no tags: it is not "unfiltered", it simply is not one of the answers.
    expect(msgs({ tags: 'net,sec', at: 'all' })).toHaveLength(3);
  });

  it('ANDs across columns while ORing within one', () => {
    expect(msgs({ kind: 'syslog,trap', tags: 'sec', at: 'all' })).toEqual([
      '%%01POLICY/6/POLICYPERMIT',
    ]);
  });
});

describe('text columns', () => {
  it('matches a substring case-insensitively in the browser', () => {
    // The client-side contract is always substring — unlike the log store, where a plain term is
    // whole-token (ADR-024's deliberate, measured divergence).
    expect(msgs({ message: 'policy', at: 'all' })).toEqual(['%%01POLICY/6/POLICYPERMIT']);
    expect(msgs({ message: 'link', at: 'all' })).toHaveLength(2);
  });

  it('partitions the rows on NOT', () => {
    const yes = msgs({ message: 'link', at: 'all' }).length;
    const no = msgs({ message: '!link', at: 'all' }).length;
    expect(yes + no).toBe(ROWS.length);
  });

  it('runs an anchored regex', () => {
    expect(msgs({ message: '~^%LINK', at: 'all' })).toEqual(['%LINK-3-UPDOWN: down']);
    expect(msgs({ message: '!~^%LINK', at: 'all' })).toHaveLength(3);
  });

  it('returns no rows for an invalid regex instead of throwing', () => {
    expect(() => msgs({ message: '~[', at: 'all' })).not.toThrow();
    expect(msgs({ message: '~[', at: 'all' })).toEqual([]);
  });
});

describe('range columns', () => {
  it('applies the window against the injected clock', () => {
    expect(msgs({ at: '6h' })).toHaveLength(2);
    expect(msgs({ at: '24h' })).toHaveLength(3);
    expect(msgs({ at: 'all' })).toHaveLength(4);
  });

  it('leaves rows alone when the list is filtered server-side', () => {
    // A server-side range is already in the query as start/end. Re-applying it here against a
    // browser clock would double-filter, and the two clocks are not the same one.
    const serverSide: FilterableColumn<Row>[] = [
      { key: 'at', filter: { ...rangeSpec, readTime: undefined } },
    ];
    expect(applyFilters(ROWS, serverSide, { at: '6h' }, NOW)).toHaveLength(4);
  });

  it('falls back to the default window when a stale URL names a preset that no longer exists', () => {
    // ⚠️ This **changed** when the range gained a custom window (ADR-053 Inc.2), and the change is
    // deliberate. It used to widen to everything, on the reasoning that a stale bookmark should not
    // hide rows. But the Events range default is a *performance contract* — bounded so that a
    // case-insensitive term stays affordable — and widening on an unrecognised token is exactly the
    // unbounded query that default exists to prevent, triggered by a URL nobody typed. Landing on
    // the default view is also what `filterParams.ts::readEnumParam` already does for every other
    // closed-set filter, so the two now agree.
    expect(msgs({ at: 'fortnight' })).toHaveLength(3);
    expect(msgs({ at: 'fortnight' })).toEqual(msgs({ at: rangeSpec.defaultPreset }));
  });
});

describe('number columns (ADR-053 Inc.6)', () => {
  interface Scored {
    name: string;
    score: number | null;
  }
  const SCORED: Scored[] = [
    { name: 'a', score: 0 },
    { name: 'b', score: 3 },
    { name: 'c', score: 5 },
    { name: 'd', score: 9 },
    { name: 'e', score: null },
  ];
  const cols: FilterableColumn<Scored>[] = [
    { key: 'score', filter: { kind: 'number', readNumber: (r) => r.score } },
  ];
  const names = (v: string) => applyFilters(SCORED, cols, { score: v }, NOW).map((r) => r.name);

  it('is inclusive at both ends', () => {
    // The one thing a numeric filter gets wrong silently: an operator asking for "3 to 5" and not
    // seeing the rows that are exactly 3 or exactly 5 reads as missing data, not as a boundary.
    expect(names('3:5')).toEqual(['b', 'c']);
    expect(names('0:0')).toEqual(['a']);
  });

  it('accepts a one-sided bound, which is the common shape', () => {
    expect(names('5:')).toEqual(['c', 'd']);
    expect(names(':3')).toEqual(['a', 'b']);
  });

  it('treats zero as a bound, not as "unset"', () => {
    // `Number('') === 0`, so the sloppy encoding of "no minimum" is indistinguishable from a real
    // minimum of zero — and on a 0-based score that is the whole list versus one row.
    expect(names('0:')).toHaveLength(4);
    expect(names(':0')).toEqual(['a']);
  });

  it('excludes a row that has no number rather than treating it as zero', () => {
    // Same rule the range kind applies to a null timestamp: a bound is a question, and a row the
    // question cannot be asked of is not an answer of "yes".
    expect(names('0:')).not.toContain('e');
    expect(names(':100')).not.toContain('e');
  });

  it('narrows nothing when unset or when both sides are unparseable', () => {
    expect(names('')).toHaveLength(SCORED.length);
    expect(names(':')).toHaveLength(SCORED.length);
    expect(names('junk')).toHaveLength(SCORED.length);
  });

  it('leaves rows alone when the list is filtered server-side', () => {
    // Exactly the `readTime: undefined` rule above: the bounds went into the query, so a second
    // pass here would narrow a page the server already narrowed.
    const serverSide: FilterableColumn<Scored>[] = [{ key: 'score', filter: { kind: 'number' } }];
    expect(applyFilters(SCORED, serverSide, { score: '9:' }, NOW)).toHaveLength(SCORED.length);
  });
});

describe('buildPredicate', () => {
  it('compiles the regex once rather than per row', () => {
    // Not observable directly; what is observable is that the two entry points agree, so callers
    // can use the cheap one over a list without changing behaviour.
    const state = { message: '~down', at: 'all' };
    const p = buildPredicate(COLUMNS, state, NOW);
    for (const row of ROWS) expect(p(row)).toBe(matchesFilters(row, COLUMNS, state, NOW));
  });

  it('is the identity when nothing narrows', () => {
    expect(applyFilters(ROWS, COLUMNS, { at: 'all' }, NOW)).toHaveLength(ROWS.length);
  });
});
