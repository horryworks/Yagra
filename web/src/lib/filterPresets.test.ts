// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import {
  CLIENT_RANGES,
  MAX_DISCOVERED_OPTIONS,
  clientRangePresets,
  discoveredOptions,
  enumOptions,
} from './filterPresets';

/** Echoes the key, so a test can see which key a label was built from. */
const t = ((key: string) => `«${key}»`) as unknown as TFunction;

const DAY = 86_400;

describe('clientRangePresets', () => {
  it('covers every declared window, in the declared order', () => {
    expect(clientRangePresets(t).map((p) => p.value)).toEqual([...CLIENT_RANGES]);
  });

  it('pins the arithmetic, because a wrong window still filters', () => {
    // This is the failure the module's own header names: `7d` written as 7×24×60×60 with a typo
    // narrows the list to the wrong period and looks exactly like a working filter. Numbers spelled
    // out rather than recomputed here — a test that repeats the implementation's expression proves
    // only that it was copied.
    const secs = Object.fromEntries(clientRangePresets(t).map((p) => [p.value, p.seconds]));
    expect(secs).toEqual({
      '24h': 86_400,
      '7d': 604_800,
      '30d': 2_592_000,
      '90d': 7_776_000,
      all: null,
    });
    expect(secs['24h']).toBe(DAY);
  });

  it('gives "all time" a null window rather than a very large one', () => {
    // `null` is what tells the caller not to send a bound at all. A huge number would send one.
    const all = clientRangePresets(t).find((p) => p.value === 'all');
    expect(all?.seconds).toBeNull();
  });

  it('labels each window from the shared key prefix', () => {
    expect(clientRangePresets(t).map((p) => p.label)).toEqual([
      '«common:filter.range.24h»',
      '«common:filter.range.7d»',
      '«common:filter.range.30d»',
      '«common:filter.range.90d»',
      '«common:filter.range.all»',
    ]);
  });
});

describe('enumOptions', () => {
  it('keeps the array as the source and builds each label under one prefix', () => {
    expect(enumOptions(['info', 'warning'] as const, t, 'sev.')).toEqual([
      { value: 'info', label: '«sev.info»' },
      { value: 'warning', label: '«sev.warning»' },
    ]);
  });

  it('is empty for an empty enum rather than throwing', () => {
    expect(enumOptions([], t, 'x.')).toEqual([]);
  });
});

describe('discoveredOptions', () => {
  const rows = (...v: (string | null | undefined)[]) => v.map((value) => ({ value }));
  const read = (r: { value: string | null | undefined }) => r.value;

  it('deduplicates and sorts', () => {
    expect(discoveredOptions(rows('beta', 'alpha', 'beta'), read).map((o) => o.value)).toEqual([
      'alpha',
      'beta',
    ]);
  });

  it('drops null, undefined and empty string — they are not values to filter by', () => {
    expect(discoveredOptions(rows(null, undefined, '', 'a'), read).map((o) => o.value)).toEqual([
      'a',
    ]);
  });

  it('passes each value through the label function when one is given', () => {
    expect(discoveredOptions(rows('a'), read, (v) => v.toUpperCase())).toEqual([
      { value: 'a', label: 'A' },
    ]);
  });

  it('labels with the value itself when no label function is given', () => {
    expect(discoveredOptions(rows('a'), read)).toEqual([{ value: 'a', label: 'a' }]);
  });

  it('holds the cap at its own number', () => {
    // The check runs after the insert, so a `>` here returned MAX+1 — a cap that does not hold at
    // the number naming it. One below the cap must be untouched, which is the half that proves the
    // guard is a cap and not a blanket truncation.
    const many = rows(...Array.from({ length: 500 }, (_, i) => `v${String(i).padStart(3, '0')}`));
    expect(discoveredOptions(many, read)).toHaveLength(MAX_DISCOVERED_OPTIONS);

    const under = rows(
      ...Array.from({ length: MAX_DISCOVERED_OPTIONS - 1 }, (_, i) => `v${i}`),
    );
    expect(discoveredOptions(under, read)).toHaveLength(MAX_DISCOVERED_OPTIONS - 1);
  });

  it('is empty for no rows', () => {
    expect(discoveredOptions([], read)).toEqual([]);
  });
});
