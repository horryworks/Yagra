// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  DUPLEX_STATES,
  duplexApplies,
  duplexEmptyReason,
  duplexState,
  IF_TYPE_ETHERNET_CSMACD,
  SPEED_TIERS,
  speedTier,
  type SpeedTier,
} from './linkMode';

describe('speedTier', () => {
  it('buckets the standard Ethernet rates exactly', () => {
    // The accepting half, and it is the load-bearing one: every mapping in this module fails
    // silently into "unknown", which looks identical to a device that reports no speed. A suite of
    // only-rejection cases would pass against a `speedTier` that returned 'unknown' for everything.
    const cases: ReadonlyArray<readonly [number, SpeedTier]> = [
      [10_000_000, '10m'],
      [100_000_000, '100m'],
      [1_000_000_000, '1g'],
      [2_500_000_000, '2_5g'],
      [5_000_000_000, '5g'],
      [10_000_000_000, '10g'],
      [25_000_000_000, '25g'],
      [40_000_000_000, '40g'],
      [100_000_000_000, '100g'],
    ];
    for (const [bps, tier] of cases) expect(speedTier(bps)).toBe(tier);
  });

  it('separates the two speeds the lab firewall actually reports', () => {
    // The finding this feature exists to surface: the WAN port negotiated 100 Mbps while the LAN
    // port beside it is at 1 Gbps. If these ever collapsed into one bucket the column would be
    // decorative.
    expect(speedTier(100_000_000)).toBe('100m');
    expect(speedTier(1_000_000_000)).toBe('1g');
    expect(speedTier(100_000_000)).not.toBe(speedTier(1_000_000_000));
  });

  it('treats a real but non-standard rate as other, not unknown', () => {
    // `Dialer1` on the lab device reports 64 kbps. That is a genuine rate and must not read as
    // "we could not tell" — the two mean different things to whoever is looking.
    expect(speedTier(64_000)).toBe('other');
    expect(speedTier(622_080_000)).toBe('other');
  });

  it('treats absent, zero and negative speeds as unknown', () => {
    for (const v of [null, undefined, 0, -1]) expect(speedTier(v)).toBe('unknown');
  });

  it('buckets every value into a declared tier', () => {
    // Guards the `?? 'other'` fallback: a value with no bucket must not leak a raw string into a
    // filter option key, which would render as the key itself.
    for (const v of [1, 7, 1_000, 3_000_000_000, Number.MAX_SAFE_INTEGER]) {
      expect(SPEED_TIERS).toContain(speedTier(v));
    }
  });
});

describe('duplexState', () => {
  it('passes the two real modes through', () => {
    expect(duplexState('full')).toBe('full');
    expect(duplexState('half')).toBe('half');
  });

  it('buckets null and any unrecognised token as unknown', () => {
    // A token this build does not know must degrade, not disappear: the row still renders.
    for (const v of [null, undefined, '', 'auto', 'Full', 'fullDuplex']) {
      expect(duplexState(v)).toBe('unknown');
    }
  });

  it('only ever answers a declared bucket', () => {
    for (const v of [null, 'full', 'half', 'nonsense']) {
      expect(DUPLEX_STATES).toContain(duplexState(v));
    }
  });
});

describe('duplexApplies', () => {
  it('applies to Ethernet and to an interface whose type is unread', () => {
    expect(duplexApplies(IF_TYPE_ETHERNET_CSMACD)).toBe(true);
    // The direction that matters: a device answering no ifType at all must read as "could not
    // read", never as "does not apply" on every row.
    expect(duplexApplies(null)).toBe(true);
    expect(duplexApplies(undefined)).toBe(true);
  });

  it('does not apply to the virtual interface types the lab device reports', () => {
    // other(1) = NULL0, ppp(23) = Dialer1, softwareLoopback(24) = InLoopBack0,
    // propVirtual(53) = Virtual-if0 — 4 of that device's 16 interfaces.
    for (const t of [1, 23, 24, 53, 131, 161]) expect(duplexApplies(t)).toBe(false);
  });
});

describe('duplexEmptyReason', () => {
  it('is null when there is a value to render', () => {
    expect(duplexEmptyReason('full', IF_TYPE_ETHERNET_CSMACD)).toBeNull();
    // Even on an interface type where it "should not" apply: if the device answered, show it.
    expect(duplexEmptyReason('half', 24)).toBeNull();
  });

  it('distinguishes "could not read" from "does not apply"', () => {
    expect(duplexEmptyReason(null, IF_TYPE_ETHERNET_CSMACD)).toBe('unknown');
    expect(duplexEmptyReason(null, 24)).toBe('notApplicable');
  });

  it('calls an unread optical-style port unknown rather than not-applicable', () => {
    // An SFP port is ethernetCsmacd, so the question applies and the honest answer is "no reading",
    // not "not applicable". Encoded here because it is the case most likely to be "fixed" wrongly.
    expect(duplexEmptyReason(null, IF_TYPE_ETHERNET_CSMACD)).toBe('unknown');
  });
});
