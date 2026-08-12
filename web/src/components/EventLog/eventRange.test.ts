// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the event log's time range (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import {
  boundsFor,
  DEFAULT_EVENT_RANGE,
  EVENT_RANGES,
  rangeIsNarrowing,
  type EventRange,
} from './eventRange';

const NOW = Date.parse('2026-08-13T12:00:00.000Z');

describe('the default range', () => {
  it('is bounded, because the search cost depends on it', () => {
    // Not a preference. A case-insensitive term costs ~1.1× over 24h and ~10× (9.4s on 6.7M events)
    // with no lower bound, so an unbounded default would silently make every search a long wait.
    expect(DEFAULT_EVENT_RANGE).not.toBe('all');
    expect(DEFAULT_EVENT_RANGE).not.toBe('custom');
    const { start, end } = boundsFor(DEFAULT_EVENT_RANGE, {}, NOW);
    expect(start).toBeDefined();
    expect(end).toBeUndefined();
  });
});

describe('boundsFor', () => {
  it('turns a preset into a lower bound and no upper bound', () => {
    expect(boundsFor('24h', {}, NOW)).toEqual({
      start: '2026-08-12T12:00:00.000Z',
      end: undefined,
    });
    expect(boundsFor('7d', {}, NOW)).toEqual({
      start: '2026-08-06T12:00:00.000Z',
      end: undefined,
    });
    expect(boundsFor('30d', {}, NOW)).toEqual({
      start: '2026-07-14T12:00:00.000Z',
      end: undefined,
    });
  });

  it('sends no bound at all for "all time"', () => {
    expect(boundsFor('all', {}, NOW)).toEqual({ start: undefined, end: undefined });
  });

  it('passes the operator\'s own instants through under custom', () => {
    expect(
      boundsFor('custom', { from: '2026-01-01T00:00:00.000Z', to: '2026-01-02T00:00:00.000Z' }, NOW),
    ).toEqual({ start: '2026-01-01T00:00:00.000Z', end: '2026-01-02T00:00:00.000Z' });
  });

  it('treats an empty custom side as unbounded rather than sending an empty string', () => {
    // `buildUrl` drops `undefined` but keeps `''`, so an empty string would reach the API as a
    // malformed timestamp and 400 — a bound nobody set turning into an error.
    expect(boundsFor('custom', { from: '', to: undefined }, NOW)).toEqual({
      start: undefined,
      end: undefined,
    });
  });

  it('ignores the custom instants while a preset is selected', () => {
    // The bar clears them when leaving Custom; this pins the behaviour even if a stale pair
    // survives, because a preset silently ANDed with a month-old upper bound would return nothing
    // and blame the preset.
    expect(boundsFor('24h', { from: '2020-01-01T00:00:00.000Z', to: '2020-01-02T00:00:00.000Z' }, NOW))
      .toEqual({ start: '2026-08-12T12:00:00.000Z', end: undefined });
  });

  it('resolves every range it offers', () => {
    for (const r of EVENT_RANGES) {
      expect(() => boundsFor(r, {}, NOW)).not.toThrow();
    }
  });
});

describe('rangeIsNarrowing', () => {
  it('counts every preset except "all time"', () => {
    const narrowing: EventRange[] = ['24h', '7d', '30d'];
    for (const r of narrowing) expect(rangeIsNarrowing(r, {})).toBe(true);
    expect(rangeIsNarrowing('all', {})).toBe(false);
  });

  it('counts custom only when an instant is actually set', () => {
    expect(rangeIsNarrowing('custom', {})).toBe(false);
    expect(rangeIsNarrowing('custom', { from: '2026-01-01T00:00:00.000Z' })).toBe(true);
    expect(rangeIsNarrowing('custom', { to: '2026-01-01T00:00:00.000Z' })).toBe(true);
  });

  it('reports the default as narrowing, because it hides older events', () => {
    expect(rangeIsNarrowing(DEFAULT_EVENT_RANGE, {})).toBe(true);
  });
});
