// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { readSources } from '../testSupport/sources';
import { keepListed } from './selection';
import { byNewestCreated } from './sort';

describe('keepListed', () => {
  it('drops ids that left the list and keeps the rest', () => {
    expect([...keepListed(new Set(['a', 'b', 'c']), ['a', 'c', 'd'])]).toEqual(['a', 'c']);
  });

  it('returns the same set when nothing dropped, so setState renders nothing', () => {
    const sel = new Set(['a']);
    expect(keepListed(sel, ['a', 'b'])).toBe(sel);
  });
});

describe('byNewestCreated', () => {
  it('sorts newest first', () => {
    const rows = [{ created_ms: 1 }, { created_ms: 3 }, { created_ms: 2 }];
    expect([...rows].sort(byNewestCreated).map((r) => r.created_ms)).toEqual([3, 2, 1]);
  });
});

/** Written once each (ADR-202). Needles assembled so this file's own text cannot match. */
describe('no screen writes the selection prune or the newest-first comparator by hand', () => {
  const HOMES: [string, string][] = [
    [`.filter((id) => ${'listed'}.has(id))`, 'lib/selection.ts'],
    [`b.created_ms - ${'a'}.created_ms`, 'lib/sort.ts'],
  ];

  it('only the lib file holds each body', () => {
    const offenders = readSources().flatMap(([p, src]) =>
      HOMES.filter(([needle, home]) => p !== home && src.includes(needle)).map(([n]) => `${p}: ${n}`),
    );
    expect(offenders, `use lib/selection or lib/sort:\n  ${offenders.join('\n  ')}`).toEqual([]);
  });

  it('finds the sources it is supposed to be reading', () => {
    const files = readSources();
    expect(files.length).toBeGreaterThan(300);
    expect(files.find(([p]) => p === 'lib/sort.ts')?.[1]).toContain(HOMES[1][0]);
    expect(files.filter(([, src]) => src.includes('keepListed(')).length).toBeGreaterThanOrEqual(3);
  });
});
