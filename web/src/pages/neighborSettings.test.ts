// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  describeCadence,
  isNeighborDirty,
  neighborFormFrom,
  parseNeighborForm,
  MAX_NEIGHBOR_INTERVAL_SECS,
  MIN_NEIGHBOR_INTERVAL_SECS,
} from './neighborSettings';

/** A `t()` stand-in that returns the key plus its interpolations, so a test can see which branch ran. */
const t = (key: string, opts?: Record<string, unknown>) => `${key}:${opts?.n ?? ''}`;

describe('parseNeighborForm', () => {
  it('accepts a whole number inside the band', () => {
    const r = parseNeighborForm({ enabled: true, intervalSecs: '3600' });
    expect(r).toEqual({ ok: true, values: { enabled: true, interval_secs: 3600 } });
  });

  it('trims whitespace', () => {
    const r = parseNeighborForm({ enabled: false, intervalSecs: '  900 ' });
    expect(r.ok && r.values.interval_secs).toBe(900);
  });

  it('rejects anything outside the band, at both edges', () => {
    for (const bad of ['0', '299', '86401', '-60']) {
      expect(parseNeighborForm({ enabled: true, intervalSecs: bad }).ok).toBe(false);
    }
    expect(parseNeighborForm({ enabled: true, intervalSecs: '300' }).ok).toBe(true);
    expect(parseNeighborForm({ enabled: true, intervalSecs: '86400' }).ok).toBe(true);
  });

  it('rejects non-integers rather than truncating them', () => {
    for (const bad of ['', 'soon', '36.5', 'NaN', '1_800']) {
      expect(parseNeighborForm({ enabled: true, intervalSecs: bad }).ok).toBe(false);
    }
    // `1e3` is 1000 — a real integer inside the band, so accepting it is correct, not a leak.
    expect(parseNeighborForm({ enabled: true, intervalSecs: '1e3' })).toEqual({
      ok: true,
      values: { enabled: true, interval_secs: 1000 },
    });
  });

  it('validates the cadence even when collection is switched off', () => {
    // The value is still stored, so letting a bad one through here would surface as a server error
    // pointing at a control the operator had just disabled.
    expect(parseNeighborForm({ enabled: false, intervalSecs: '5' }).ok).toBe(false);
  });

  it('prefers the band the server reported over the compiled mirror', () => {
    // The server is authoritative; if a deployment ever widens the band, the form must not refuse
    // a value the server would accept.
    const r = parseNeighborForm({ enabled: true, intervalSecs: '60' }, { min: 60, max: 7200 });
    expect(r.ok).toBe(true);
    expect(parseNeighborForm({ enabled: true, intervalSecs: '7201' }, { min: 60, max: 7200 }).ok).toBe(
      false,
    );
  });

  it('falls back to the mirror when the server reported nothing usable', () => {
    for (const band of [undefined, { min: null, max: null }, { min: 0, max: 0 }]) {
      const r = parseNeighborForm({ enabled: true, intervalSecs: '60' }, band);
      expect(r).toEqual({
        ok: false,
        min: MIN_NEIGHBOR_INTERVAL_SECS,
        max: MAX_NEIGHBOR_INTERVAL_SECS,
      });
    }
  });
});

describe('isNeighborDirty', () => {
  const saved = { enabled: true, interval_secs: 3600 };

  it('is clean for the value the server reported', () => {
    expect(isNeighborDirty(neighborFormFrom(saved), saved)).toBe(false);
  });

  it('notices either half changing', () => {
    expect(isNeighborDirty({ enabled: false, intervalSecs: '3600' }, saved)).toBe(true);
    expect(isNeighborDirty({ enabled: true, intervalSecs: '1800' }, saved)).toBe(true);
  });

  it('does not treat re-typed whitespace as an edit', () => {
    expect(isNeighborDirty({ enabled: true, intervalSecs: ' 3600 ' }, saved)).toBe(false);
  });
});

describe('describeCadence', () => {
  it('renders whole hours and whole minutes in their own units', () => {
    expect(describeCadence(3600, t)).toBe('settings.neighbors.cadence.hours:1');
    expect(describeCadence(86400, t)).toBe('settings.neighbors.cadence.hours:24');
    expect(describeCadence(900, t)).toBe('settings.neighbors.cadence.minutes:15');
  });

  it('keeps anything else in seconds rather than rounding it into a lie', () => {
    expect(describeCadence(3665, t)).toBe('settings.neighbors.cadence.seconds:3665');
    expect(describeCadence(301, t)).toBe('settings.neighbors.cadence.seconds:301');
  });

  it('does not crash on a nonsense value from a newer server', () => {
    expect(describeCadence(0, t)).toBe('settings.neighbors.cadence.seconds:0');
    expect(describeCadence(Number.NaN, t)).toBe('settings.neighbors.cadence.seconds:0');
  });
});
