// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { readSources } from '../testSupport/sources';
import { appendPage, nextCursorFrom } from './keysetPage';

describe('nextCursorFrom', () => {
  const rows = [
    { id: 'a', at: '2026-01-01T00:00:02Z' },
    { id: 'b', at: '2026-01-01T00:00:01Z' },
  ];

  it('reads the cursor from the last row of a full page', () => {
    expect(nextCursorFrom(rows, 2, (r) => ({ before: r.at, before_id: r.id }))).toEqual({
      before: '2026-01-01T00:00:01Z',
      before_id: 'b',
    });
  });

  it('answers null for a short page and for an empty one', () => {
    expect(nextCursorFrom(rows, 3, (r) => r.at)).toBeNull();
    expect(nextCursorFrom([], 0, (r: { at: string }) => r.at)).toBeNull();
  });
});

describe('appendPage', () => {
  it('keeps the order and drops a row already held, for string and number ids', () => {
    expect(appendPage([{ id: 'a' }, { id: 'b' }], [{ id: 'b' }, { id: 'c' }])).toEqual([
      { id: 'a' },
      { id: 'b' },
      { id: 'c' },
    ]);
    expect(appendPage([{ id: 1 }], [{ id: 1 }, { id: 2 }])).toEqual([{ id: 1 }, { id: 2 }]);
  });
});

/**
 * The two paging rules are written once (ADR-202). Each list keeps a one-line `nextCursor` naming
 * its own cursor fields, and re-exports `appendPage`; neither body may be written out again.
 */
describe('no list writes the keyset paging rule by hand', () => {
  // Assembled so this file's own text cannot match.
  const NEEDLES = [`new Set(${'have'}.map(`, `rows.length < ${'PAGE_SIZE'}`];

  it('only lib/keysetPage.ts holds either body', () => {
    const offenders = readSources()
      .filter(([p, src]) => p !== 'lib/keysetPage.ts' && NEEDLES.some((n) => src.includes(n)))
      .map(([p]) => p);
    expect(offenders, `use lib/keysetPage:\n  ${offenders.join('\n  ')}`).toEqual([]);
  });

  it('finds the sources it is supposed to be reading', () => {
    const files = readSources();
    expect(files.length).toBeGreaterThan(300);
    const own = files.find(([p]) => p === 'lib/keysetPage.ts')?.[1] ?? '';
    expect(own.includes(`new Set(${'have'}.map(`)).toBe(true);
    expect(files.filter(([, src]) => src.includes('nextCursorFrom(')).length).toBeGreaterThanOrEqual(4);
  });
});
