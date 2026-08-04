// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  describeCadence,
  discoveryFormFrom,
  isDiscoveryDirty,
  parseDiscoveryForm,
  DISCOVERY_WALKS,
  MAX_NEIGHBOR_INTERVAL_SECS,
  MIN_NEIGHBOR_INTERVAL_SECS,
  type DiscoveryForm,
} from './neighborSettings';
import type { NeighborConfig } from '../types/api';

/** A `t()` stand-in that returns the key plus its interpolations, so a test can see which branch ran. */
const t = (key: string, opts?: Record<string, unknown>) => `${key}:${opts?.n ?? ''}`;

const SAVED: NeighborConfig = {
  enabled: true,
  interval_secs: 3600,
  l3_enabled: true,
  l3_interval_secs: 3600,
  arp_enabled: false,
  arp_interval_secs: 21600,
  routing_enabled: true,
  routing_interval_secs: 3600,
  min_interval_secs: MIN_NEIGHBOR_INTERVAL_SECS,
  max_interval_secs: MAX_NEIGHBOR_INTERVAL_SECS,
};

/** A valid form with one field overridden, so each test names only what it is about. */
function form(over: Partial<DiscoveryForm> = {}): DiscoveryForm {
  return { ...discoveryFormFrom(SAVED), ...over };
}

describe('discoveryFormFrom', () => {
  it('carries every walk, so no control renders empty', () => {
    const f = discoveryFormFrom(SAVED);
    expect(Object.keys(f).sort()).toEqual([...DISCOVERY_WALKS].sort());
    expect(f.arp).toEqual({ enabled: false, intervalSecs: '21600' });
    expect(f.l3).toEqual({ enabled: true, intervalSecs: '3600' });
  });

  it('reads a null from an older server as off rather than as on', () => {
    // A server that predates a walk reports `null`. Rendering that as a checked box would claim a
    // walk is running that the server has never heard of.
    const old = { ...SAVED, arp_enabled: null, arp_interval_secs: null } as NeighborConfig;
    expect(discoveryFormFrom(old).arp.enabled).toBe(false);
    expect(discoveryFormFrom(old).arp.intervalSecs).toBe(String(MIN_NEIGHBOR_INTERVAL_SECS));
  });
});

describe('parseDiscoveryForm', () => {
  it('accepts whole numbers inside the band and sends every field', () => {
    const r = parseDiscoveryForm(form());
    // Every field, not just the edited one: the server treats an absent field as "leave it", which
    // is right for an old client and would make this one's saves half-apply.
    expect(r).toEqual({
      ok: true,
      values: {
        enabled: true,
        interval_secs: 3600,
        l3_enabled: true,
        l3_interval_secs: 3600,
        arp_enabled: false,
        arp_interval_secs: 21600,
        routing_enabled: true,
        routing_interval_secs: 3600,
      },
    });
  });

  it('trims whitespace', () => {
    const r = parseDiscoveryForm(form({ arp: { enabled: true, intervalSecs: '  900 ' } }));
    expect(r.ok && r.values.arp_interval_secs).toBe(900);
  });

  it('rejects anything outside the band, at both edges, for every walk', () => {
    for (const walk of DISCOVERY_WALKS) {
      for (const bad of ['0', '299', '86401', '-60']) {
        const r = parseDiscoveryForm(form({ [walk]: { enabled: true, intervalSecs: bad } }));
        expect(r.ok).toBe(false);
        // The error must name the walk whose control is wrong, or the operator reads it as being
        // about whichever control they last touched.
        expect(!r.ok && r.walk).toBe(walk);
      }
    }
    expect(parseDiscoveryForm(form({ arp: { enabled: true, intervalSecs: '300' } })).ok).toBe(true);
    expect(parseDiscoveryForm(form({ arp: { enabled: true, intervalSecs: '86400' } })).ok).toBe(true);
  });

  it('rejects non-integers rather than truncating them', () => {
    for (const bad of ['', 'soon', '36.5', 'NaN', '1_800']) {
      expect(parseDiscoveryForm(form({ l3: { enabled: true, intervalSecs: bad } })).ok).toBe(false);
    }
    // `1e3` is 1000 — a real integer inside the band, so accepting it is correct, not a leak.
    const r = parseDiscoveryForm(form({ l3: { enabled: true, intervalSecs: '1e3' } }));
    expect(r.ok && r.values.l3_interval_secs).toBe(1000);
  });

  it('validates a cadence even when its walk is switched off', () => {
    // The value is still stored, so letting a bad one through here would surface as a server error
    // pointing at a control the operator had just disabled.
    expect(parseDiscoveryForm(form({ arp: { enabled: false, intervalSecs: '5' } })).ok).toBe(false);
  });

  /** Every walk set to the same cadence, so a band test is about the band and not about the ARP
   *  default sitting outside a narrowed one. */
  function allAt(secs: string): DiscoveryForm {
    return {
      neighbors: { enabled: true, intervalSecs: secs },
      l3: { enabled: true, intervalSecs: secs },
      arp: { enabled: false, intervalSecs: secs },
      routing: { enabled: true, intervalSecs: secs },
    };
  }

  it('prefers the band the server reported over the compiled mirror', () => {
    // The server is authoritative; if a deployment ever widens the band, the form must not refuse a
    // value the server would accept.
    expect(parseDiscoveryForm(allAt('60'), { min: 60, max: 7200 }).ok).toBe(true);
    expect(parseDiscoveryForm(allAt('7201'), { min: 60, max: 7200 }).ok).toBe(false);
  });

  it('applies the narrowed band to every walk, not only the first', () => {
    // The shipped ARP default (21600) is outside a band of 60–7200. A validator that checked only
    // the adjacency pair would pass this form and then fail server-side, pointing at a control the
    // operator never touched.
    const onlyArpOut = form({
      neighbors: { enabled: true, intervalSecs: '3600' },
      l3: { enabled: true, intervalSecs: '3600' },
    });
    const r = parseDiscoveryForm(onlyArpOut, { min: 60, max: 7200 });
    expect(r.ok).toBe(false);
    expect(!r.ok && r.walk).toBe('arp');
  });

  it('falls back to the mirror when the server reported nothing usable', () => {
    for (const band of [undefined, { min: null, max: null }, { min: 0, max: 0 }]) {
      expect(parseDiscoveryForm(allAt('60'), band)).toEqual({
        ok: false,
        walk: 'neighbors',
        min: MIN_NEIGHBOR_INTERVAL_SECS,
        max: MAX_NEIGHBOR_INTERVAL_SECS,
      });
    }
  });
});

describe('isDiscoveryDirty', () => {
  it('is clean for the values the server reported', () => {
    expect(isDiscoveryDirty(discoveryFormFrom(SAVED), SAVED)).toBe(false);
  });

  it('notices either half of any walk changing', () => {
    for (const walk of DISCOVERY_WALKS) {
      const was = discoveryFormFrom(SAVED)[walk];
      expect(
        isDiscoveryDirty(form({ [walk]: { ...was, enabled: !was.enabled } }), SAVED),
      ).toBe(true);
      expect(isDiscoveryDirty(form({ [walk]: { ...was, intervalSecs: '1800' } }), SAVED)).toBe(true);
    }
  });

  it('does not treat re-typed whitespace as an edit', () => {
    expect(
      isDiscoveryDirty(form({ neighbors: { enabled: true, intervalSecs: ' 3600 ' } }), SAVED),
    ).toBe(false);
  });
});

describe('describeCadence', () => {
  it('renders whole hours and whole minutes in their own units', () => {
    expect(describeCadence(3600, t)).toBe('settings.neighbors.cadence.hours:1');
    expect(describeCadence(86400, t)).toBe('settings.neighbors.cadence.hours:24');
    expect(describeCadence(900, t)).toBe('settings.neighbors.cadence.minutes:15');
    // The ARP default, which is the one an operator reads most often on this card.
    expect(describeCadence(21600, t)).toBe('settings.neighbors.cadence.hours:6');
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
