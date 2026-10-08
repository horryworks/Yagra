// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { idleLegendIdx, timeLegend } from './legend';

describe('idleLegendIdx', () => {
  it('returns the last index when every series is filled', () => {
    expect(idleLegendIdx([[1, 2, 3], [4, 5, 6]])).toBe(2);
  });

  // The reason this exists at all: a window that runs to `now` ends in a bucket no poll has
  // filled, so "the last column" is a gap and reading it puts the `--` straight back.
  it('skips a trailing gap shared by every series', () => {
    expect(idleLegendIdx([[1, 2, null], [4, 5, null]])).toBe(1);
  });

  // A series that started later (interface packet rates, collected only since the upgrade that
  // added them) must not drag the readout back to where the OLDEST series stops.
  it('takes the most recent sample any series has, not the oldest series’ last one', () => {
    expect(idleLegendIdx([[1, null, null], [null, null, 9]])).toBe(2);
  });

  it('ignores a series that is entirely gaps', () => {
    expect(idleLegendIdx([[null, null, null], [7, null, null]])).toBe(0);
  });

  it('returns null when there is nothing to report', () => {
    expect(idleLegendIdx([])).toBeNull();
    expect(idleLegendIdx([[], []])).toBeNull();
    expect(idleLegendIdx([[null, null]])).toBeNull();
  });

  // uPlot renders 0 as a value, not as a gap — and a flat zero is the NORMAL reading for the
  // errors/discards chart, so treating it as "nothing here" would blank the legend on every
  // healthy interface.
  it('treats zero as a value, not a gap', () => {
    expect(idleLegendIdx([[5, 0]])).toBe(1);
  });
});

describe('timeLegend', () => {
  // uPlot's own default for the x row is the English word "Time" and a US clock, whatever the
  // interface language is — the row has to be named and formatted by the caller.
  it('names the row with the label it is given', () => {
    expect(timeLegend('時刻', String).label).toBe('時刻');
  });

  it('formats the seconds uPlot hands it as milliseconds', () => {
    const seen: number[] = [];
    const row = timeLegend('Time', (ms) => {
      seen.push(ms);
      return 'x';
    });
    expect(row.value(null, 1_700_000_000)).toBe('x');
    expect(seen).toEqual([1_700_000_000_000]);
  });

  it('reads a missing instant as --, not as the epoch', () => {
    expect(timeLegend('Time', String).value(null, null)).toBe('--');
  });
});
