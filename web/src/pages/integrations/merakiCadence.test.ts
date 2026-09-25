// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  CADENCE_FAST_MAX_SECS,
  CADENCE_FAST_MIN_SECS,
  CADENCE_INVENTORY_MAX_SECS,
  CADENCE_INVENTORY_MIN_SECS,
  CADENCE_SWITCH_PORTS_MAX_SECS,
  CADENCE_SWITCH_PORTS_MIN_SECS,
  CADENCE_TRAFFIC_MAX_SECS,
  CADENCE_TRAFFIC_MIN_SECS,
  MERAKI_CADENCE_BOUNDS,
  MERAKI_CADENCE_FIELDS,
  CADENCE_TARGET_RPS_MAX,
  cadenceRange,
  parseCadence,
  parseTargetRps,
} from './merakiCadence';

describe('the cadence bounds', () => {
  // Whether these are the numbers the SERVER accepts is a Rust test's job
  // (`the_cadence_bounds_the_webui_shows_are_the_ones_this_api_accepts`); what is pinned here is
  // that each interval is held to the band it belongs to, and that the hint is that band.
  it('holds availability and uplink to the fast band, and the slow two to their own', () => {
    const fast = { min: CADENCE_FAST_MIN_SECS, max: CADENCE_FAST_MAX_SECS };
    expect(MERAKI_CADENCE_BOUNDS.availability).toEqual(fast);
    expect(MERAKI_CADENCE_BOUNDS.uplink).toEqual(fast);
    expect(MERAKI_CADENCE_BOUNDS.traffic).toEqual({
      min: CADENCE_TRAFFIC_MIN_SECS,
      max: CADENCE_TRAFFIC_MAX_SECS,
    });
    expect(MERAKI_CADENCE_BOUNDS.inventory).toEqual({
      min: CADENCE_INVENTORY_MIN_SECS,
      max: CADENCE_INVENTORY_MAX_SECS,
    });
    // ADR-167: the switch ports have a band of their own.
    expect(MERAKI_CADENCE_BOUNDS.switch_ports).toEqual({
      min: CADENCE_SWITCH_PORTS_MIN_SECS,
      max: CADENCE_SWITCH_PORTS_MAX_SECS,
    });
  });

  it('reads each hint out of the bounds it describes', () => {
    expect(cadenceRange('availability')).toBe('60–3600');
    expect(cadenceRange('uplink')).toBe('60–3600');
    expect(cadenceRange('traffic')).toBe('300–86400');
    expect(cadenceRange('switch_ports')).toBe('300–600');
    // The floor that moved: 900 while the tier did nothing, 60 since the sync rides on it.
    expect(cadenceRange('inventory')).toBe('60–604800');
  });

  it('never offers a band that is empty or upside down', () => {
    for (const field of MERAKI_CADENCE_FIELDS) {
      const { min, max } = MERAKI_CADENCE_BOUNDS[field];
      expect(min, field).toBeGreaterThan(0);
      expect(max, field).toBeGreaterThan(min);
    }
  });
});

describe('what a cadence box may be saved with (ADR-164 増分 18)', () => {
  it('refuses an emptied box, which used to be sent as 0', () => {
    expect(parseCadence('availability', '')).toBeNull();
    expect(parseCadence('availability', '   ')).toBeNull();
    expect(parseTargetRps('')).toBeNull();
  });

  it('holds each interval to its own band', () => {
    expect(parseCadence('availability', String(CADENCE_FAST_MIN_SECS))).toBe(CADENCE_FAST_MIN_SECS);
    expect(parseCadence('availability', String(CADENCE_FAST_MIN_SECS - 1))).toBeNull();
    expect(parseCadence('traffic', String(CADENCE_TRAFFIC_MAX_SECS + 1))).toBeNull();
    expect(parseCadence('traffic', '1.5')).toBeNull();
  });

  it('takes a fractional rate above zero, up to the ceiling', () => {
    expect(parseTargetRps('0.5')).toBe(0.5);
    expect(parseTargetRps('0')).toBeNull();
    expect(parseTargetRps(String(CADENCE_TARGET_RPS_MAX))).toBe(CADENCE_TARGET_RPS_MAX);
    expect(parseTargetRps(String(CADENCE_TARGET_RPS_MAX + 1))).toBeNull();
    expect(parseTargetRps('-1')).toBeNull();
  });
});
