// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import {
  pendingAddresses,
  mergePreview,
  destinationLabel,
  importMessage,
  DESTINATION_KINDS,
  type RowDestination,
} from './importFiling';
import type { ImportPreview, ImportResult } from '../types/api';

const preview = (p: Partial<ImportPreview>): ImportPreview => ({
  matched: [],
  ambiguous: [],
  unmatched: [],
  any_prefixes: true,
  ...p,
});

const labelOpts = {
  filing: true,
  fallbackPath: 'Matsuyama Home',
  rootLabel: 'Tree root',
  pendingLabel: '…',
  pathOf: (id: string) => `path/${id}`,
};

describe('pendingAddresses', () => {
  it('asks only for addresses with no answer yet', () => {
    const known = new Map<string, RowDestination>([['10.0.0.1', { kind: 'unmatched' }]]);
    expect(
      pendingAddresses(known, [{ address: '10.0.0.1' }, { address: '10.0.0.2' }]),
    ).toEqual(['10.0.0.2']);
  });

  it('does not ask for the same new address twice', () => {
    expect(
      pendingAddresses(new Map(), [{ address: '10.0.0.2' }, { address: '10.0.0.2' }]),
    ).toEqual(['10.0.0.2']);
  });

  it('asks for nothing when every candidate is answered', () => {
    const known = new Map<string, RowDestination>([['10.0.0.1', { kind: 'unmatched' }]]);
    expect(pendingAddresses(known, [{ address: '10.0.0.1' }])).toEqual([]);
  });
});

describe('mergePreview', () => {
  it('keys all three answers by address and keeps earlier ones', () => {
    const first = mergePreview(
      new Map(),
      preview({ unmatched: ['10.0.0.9'] }),
    );
    const merged = mergePreview(
      first,
      preview({
        matched: [{ address: '192.168.1.1', group_id: 'g1', prefix: '192.168.1.0/24' }],
        ambiguous: [{ address: '10.5.0.1', group_ids: ['g1', 'g2'] }],
      }),
    );
    expect(merged.get('10.0.0.9')).toEqual({ kind: 'unmatched' });
    expect(merged.get('192.168.1.1')).toEqual({
      kind: 'matched',
      groupId: 'g1',
      prefix: '192.168.1.0/24',
    });
    expect(merged.get('10.5.0.1')).toEqual({ kind: 'ambiguous', groupIds: ['g1', 'g2'] });
  });

  it('is idempotent — the same response twice changes nothing', () => {
    const p = preview({
      matched: [{ address: '192.168.1.1', group_id: 'g1', prefix: '192.168.1.0/24' }],
    });
    const once = mergePreview(new Map(), p);
    const twice = mergePreview(once, p);
    expect([...twice.entries()]).toEqual([...once.entries()]);
  });

  it('does not mutate the map it was given', () => {
    const known = new Map<string, RowDestination>();
    mergePreview(known, preview({ unmatched: ['10.0.0.1'] }));
    expect(known.size).toBe(0);
  });
});

describe('destinationLabel', () => {
  it('reads the fallback on a row with no answer when filing is off', () => {
    expect(destinationLabel(undefined, { ...labelOpts, filing: false })).toEqual({
      primary: 'Matsuyama Home',
    });
  });

  // 🚨 The defect this pins, found by an operator on a real sweep and by no test: with the option
  // off, a row the server had already matched to a site printed only "Tree root". The answer was
  // on hand and thrown away, and the cell read as "no folder owns this address" rather than as
  // "you have not ticked the box".
  it('still names the folder that claims a row when filing is off', () => {
    const off = destinationLabel(
      { kind: 'matched', groupId: 'g1', prefix: '192.168.1.0/24' },
      { ...labelOpts, filing: false, fallbackPath: null },
    );
    expect(off.primary).toBe('Tree root');
    expect(off.whyKey).toBe('discovery.dest.why.wouldMatch');
    expect(off.whyArgs).toEqual({ folder: 'path/g1', prefix: '192.168.1.0/24' });
  });

  it('offers nothing extra when filing is off and nothing claims the row', () => {
    // An unmatched or contested row lands in the fallback either way, so there is nothing the
    // operator is missing by leaving the box unticked — and a hint there would be noise.
    const cases: RowDestination[] = [
      { kind: 'unmatched' },
      { kind: 'ambiguous', groupIds: ['g1', 'g2'] },
    ];
    for (const dest of cases) {
      expect(destinationLabel(dest, { ...labelOpts, filing: false })).toEqual({
        primary: 'Matsuyama Home',
      });
    }
  });

  it('names the tree root when no folder was chosen', () => {
    expect(
      destinationLabel(undefined, { ...labelOpts, filing: false, fallbackPath: null }),
    ).toEqual({ primary: 'Tree root' });
  });

  it('shows the pending marker while the answer is in flight', () => {
    expect(destinationLabel(undefined, labelOpts).primary).toBe('…');
  });

  it('names the matched folder and the range that matched', () => {
    expect(
      destinationLabel({ kind: 'matched', groupId: 'g1', prefix: '192.168.1.0/24' }, labelOpts),
    ).toEqual({
      primary: 'path/g1',
      whyKey: 'discovery.dest.why.matched',
      whyArgs: { prefix: '192.168.1.0/24' },
    });
  });

  // 🚨 The two fallback cases must not read alike: one is "outside every range", the other is
  // "inside two", and only the second is a thing to go and fix.
  it('distinguishes a contested address from one no range covers', () => {
    const ambiguous = destinationLabel(
      { kind: 'ambiguous', groupIds: ['g1', 'g2'] },
      labelOpts,
    );
    const unmatched = destinationLabel({ kind: 'unmatched' }, labelOpts);
    expect(ambiguous.primary).toBe('Matsuyama Home');
    expect(unmatched.primary).toBe('Matsuyama Home');
    expect(ambiguous.whyKey).toBe('discovery.dest.why.ambiguous');
    expect(ambiguous.whyArgs).toEqual({ count: 2 });
    expect(unmatched.whyKey).toBe('discovery.dest.why.unmatched');
    expect(ambiguous.whyKey).not.toBe(unmatched.whyKey);
  });

  it('has a label for every kind it declares', () => {
    const byKind: Record<(typeof DESTINATION_KINDS)[number], RowDestination> = {
      matched: { kind: 'matched', groupId: 'g1', prefix: '10.0.0.0/8' },
      ambiguous: { kind: 'ambiguous', groupIds: ['g1', 'g2'] },
      unmatched: { kind: 'unmatched' },
    };
    for (const kind of DESTINATION_KINDS) {
      expect(destinationLabel(byKind[kind], labelOpts).primary).toBeTruthy();
    }
  });
});

describe('importMessage', () => {
  const filed = (m: number, a: number, u: number, c = 0): ImportResult => ({
    created: m + a + u + c,
    filed: { matched: m, ambiguous: a, unmatched: u, chosen: c },
  });

  it('says nothing about filing when the option was off', () => {
    expect(importMessage({ created: 3 }, 'Matsuyama Home')).toEqual([
      { key: 'discovery.msg.importedInto', args: { count: 3, site: 'Matsuyama Home' } },
    ]);
  });

  it('names the root when no folder was chosen and filing was off', () => {
    expect(importMessage({ created: 3 }, null)).toEqual([
      { key: 'discovery.msg.imported', args: { count: 3 } },
    ]);
  });

  it('reports how many were filed and how many fell back', () => {
    const parts = importMessage(filed(11, 0, 3), 'Matsuyama Home');
    expect(parts[0]).toEqual({
      key: 'discovery.msg.importedFiled',
      args: { count: 14, filed: 11 },
    });
    expect(parts[1]).toEqual({
      key: 'discovery.msg.fellBackInto',
      args: { count: 3, site: 'Matsuyama Home' },
    });
    expect(parts).toHaveLength(2);
  });

  // 🚨 The failure this exists for: "3 fell back" alone reads as three addresses outside every
  // range, when one of them was claimed by two folders.
  it('adds the contested sentence only when something was contested', () => {
    const quiet = importMessage(filed(11, 0, 3), null);
    const contested = importMessage(filed(11, 1, 2), null);
    expect(quiet.some((p) => p.key === 'discovery.msg.contested')).toBe(false);
    expect(contested.some((p) => p.key === 'discovery.msg.contested')).toBe(true);
    expect(contested.find((p) => p.key === 'discovery.msg.contested')?.args).toEqual({
      count: 1,
    });
  });

  // 🚨 A row the operator directed is not the rule succeeding. Folding it into `matched` would
  // report the rule as having decided something a person decided (ADR-131 決定 11).
  it('reports rows the operator directed as their own sentence', () => {
    const quiet = importMessage(filed(2, 0, 0), null);
    const directed = importMessage(filed(2, 0, 0, 3), null);
    expect(quiet.some((p) => p.key === 'discovery.msg.chosen')).toBe(false);
    const part = directed.find((p) => p.key === 'discovery.msg.chosen');
    expect(part?.args).toEqual({ count: 3 });
    // …and it does not inflate the rule's own count.
    expect(directed[0].args).toEqual({ count: 5, filed: 2 });
  });

  it('omits the fallback sentence when everything was filed', () => {
    const parts = importMessage(filed(4, 0, 0), 'Matsuyama Home');
    expect(parts).toHaveLength(1);
    expect(parts[0].key).toBe('discovery.msg.importedFiled');
  });
});
