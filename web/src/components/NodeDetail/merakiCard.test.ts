// SPDX-License-Identifier: AGPL-3.0-only
// The Overview's Cisco Meraki card (ADR-164 増分 13): what it draws per WAN uplink.

import { describe, expect, it } from 'vitest';
import { MERAKI_PAIR_STATES, type MerakiPair } from '../../types/api';
import {
  MERAKI_UPLINK_STATES,
  merakiPairLine,
  merakiRadioLines,
  merakiTrafficSeries,
  merakiUplinkLines,
  merakiUplinkState,
  merakiVpnLine,
} from './merakiCard';

const row = (r: number, value: number, name?: string) => ({ row: r, value, name });

describe('merakiUplinkLines', () => {
  it('joins send and receive by uplink, in row order, with the names the readings carry', () => {
    expect(
      merakiUplinkLines(
        [row(2, 10, 'WAN2'), row(1, 5_000, 'WAN1')],
        [row(1, 10_000, 'WAN1'), row(2, 20, 'WAN2')],
      ),
    ).toEqual([
      { row: 1, name: 'WAN1', state: null, sentBps: 5_000, recvBps: 10_000 },
      { row: 2, name: 'WAN2', state: null, sentBps: 10, recvBps: 20 },
    ]);
  });

  it('keeps an uplink that reported only one direction, without shifting its neighbour', () => {
    expect(merakiUplinkLines([row(1, 1, 'WAN1'), row(3, 3, 'cellular')], [row(3, 30, 'cellular')])).toEqual([
      { row: 1, name: 'WAN1', state: null, sentBps: 1, recvBps: null },
      { row: 3, name: 'cellular', state: null, sentBps: 3, recvBps: 30 },
    ]);
  });

  it('keeps an idle uplink: zero is a reading', () => {
    expect(merakiUplinkLines([row(2, 0, 'WAN2')], [row(2, 0, 'WAN2')])).toEqual([
      { row: 2, name: 'WAN2', state: null, sentBps: 0, recvBps: 0 },
    ]);
  });

  it('calls a row nobody named by its key, and takes a name from any metric', () => {
    expect(merakiUplinkLines([row(4, 1)], [])).toEqual([
      { row: 4, name: '#4', state: null, sentBps: 1, recvBps: null },
    ]);
    expect(merakiUplinkLines([row(1, 1)], [row(1, 2, 'WAN1')])[0].name).toBe('WAN1');
    expect(merakiUplinkLines([], [], [row(2, 0, 'WAN2')])[0].name).toBe('WAN2');
  });

  it('gives each uplink its state, and an uplink that reported only a state still gets a line', () => {
    // WAN2 unused (no line), cellular failed: neither carries traffic, both are shown.
    expect(
      merakiUplinkLines(
        [row(1, 5, 'WAN1')],
        [row(1, 6, 'WAN1')],
        [row(1, 2, 'WAN1'), row(2, 0, 'WAN2'), row(3, -1, 'cellular')],
      ),
    ).toEqual([
      { row: 1, name: 'WAN1', state: 'active', sentBps: 5, recvBps: 6 },
      { row: 2, name: 'WAN2', state: 'notConnected', sentBps: null, recvBps: null },
      { row: 3, name: 'cellular', state: 'failed', sentBps: null, recvBps: null },
    ]);
  });

  it('draws nothing when no uplink reported', () => {
    expect(merakiUplinkLines([], [])).toEqual([]);
  });
});

describe('merakiUplinkState', () => {
  it("reads the collector's encoding (ADR-164 決定 24): failed is below not connected", () => {
    expect(merakiUplinkState(2)).toBe('active');
    expect(merakiUplinkState(1)).toBe('ready');
    expect(merakiUplinkState(0)).toBe('notConnected');
    expect(merakiUplinkState(-1)).toBe('failed');
  });

  it('says nothing about a value outside the encoding, rather than guessing', () => {
    for (const v of [0.5, 3, -2, NaN, null, undefined]) expect(merakiUplinkState(v)).toBeNull();
  });

  it('every state is one the encoding can produce', () => {
    const produced = [2, 1, 0, -1].map(merakiUplinkState);
    expect([...produced].sort()).toEqual([...MERAKI_UPLINK_STATES].sort());
  });
});

describe('merakiVpnLine', () => {
  it('a spoke with both hubs, one, and none — the three tones the seeded rule draws', () => {
    expect(merakiVpnLine(2, 0, null)).toEqual({ reached: 2, total: 2, spokesDown: null, tone: 'ok' });
    expect(merakiVpnLine(1, 1, null)).toEqual({ reached: 1, total: 2, spokesDown: null, tone: 'warning' });
    expect(merakiVpnLine(0, 2, null)).toEqual({ reached: 0, total: 2, spokesDown: null, tone: 'critical' });
  });

  it('a hub counts its hubs and its unreachable spokes, zero included', () => {
    expect(merakiVpnLine(11, 0, 0)).toEqual({ reached: 11, total: 11, spokesDown: 0, tone: 'ok' });
    expect(merakiVpnLine(11, 0, 3)?.spokesDown).toBe(3);
  });

  it('no reading draws no line, rather than a guessed "fine"', () => {
    // A down MX, or one whose only hub is down, is not reported at all (ADR-164 決定 25).
    expect(merakiVpnLine(null, null, null)).toBeNull();
    expect(merakiVpnLine(1, null, null)).toBeNull();
  });
});

describe('merakiPairLine', () => {
  const pair = (state: MerakiPair['state'], partner: MerakiPair['partner'] = null): MerakiPair => ({
    role: 'primary',
    state,
    partner,
  });

  it('an MX with no pair draws no line', () => {
    expect(merakiPairLine(null)).toBeNull();
    expect(merakiPairLine(undefined)).toBeNull();
  });

  it('names the partner the server named, and links it only when it is a node', () => {
    const line = merakiPairLine(
      pair('normal', { name: 'mx-b', role: 'spare', node_id: 'n-2', node_state: 'ok' }),
    );
    expect(line).toEqual({
      role: 'primary',
      state: 'normal',
      partner: { name: 'mx-b', role: 'spare', nodeId: 'n-2' },
      tone: 'ok',
      vpnNotRead: false,
    });
    expect(merakiPairLine(pair('unknown', { name: 'mx-b' }))?.partner).toEqual({
      name: 'mx-b',
      role: null,
      nodeId: null,
    });
  });

  it('colours each state, and leaves unknown uncoloured rather than calling it fine', () => {
    const tones = Object.fromEntries(
      MERAKI_PAIR_STATES.map((s) => [s, merakiPairLine(pair(s))?.tone]),
    );
    expect(tones).toEqual({
      normal: 'ok',
      running_on_spare: 'warning',
      spare_down: 'warning',
      both_down: 'critical',
      unknown: null,
    });
  });

  it('says VPN is not read while the site runs on its spare, and only then', () => {
    // Whether or not a reading is on the card: one from before the failover is stale, and the
    // latest-value read keeps it for 30 minutes.
    expect(merakiPairLine(pair('running_on_spare'))?.vpnNotRead).toBe(true);
    // A spare in a normal pair has no VPN line of its own either, but that is not "not readable".
    for (const s of MERAKI_PAIR_STATES.filter((s) => s !== 'running_on_spare')) {
      expect(merakiPairLine(pair(s))?.vpnNotRead).toBe(false);
    }
  });
});

describe('merakiTrafficSeries (ADR-164 決定 27)', () => {
  const PAL = ['c0', 'c1', 'c2'];
  const L = { sent: 'sent', recv: 'received' };
  const pts = (...pairs: [number, number][]) => pairs.map(([t, v]) => ({ t, v }));

  it('draws sending above zero and receiving below it, one colour per uplink', () => {
    const { timestamps, series } = merakiTrafficSeries(
      [
        { row: 1, name: 'WAN1', sent: pts([60, 100], [120, 200]), recv: pts([60, 10], [120, 20]) },
        { row: 2, name: 'WAN2', sent: pts([60, 5]), recv: pts([60, 7]) },
      ],
      PAL,
      L,
    );
    expect(timestamps).toEqual([60, 120]);
    expect(series).toEqual([
      { label: 'WAN1 sent', values: [100, 200], color: 'c0' },
      { label: 'WAN1 received', values: [-10, -20], color: 'c0' },
      { label: 'WAN2 sent', values: [5, null], color: 'c1' },
      { label: 'WAN2 received', values: [-7, null], color: 'c1' },
    ]);
  });

  it('places each point by its timestamp, and keeps a gap a gap rather than a zero', () => {
    const { timestamps, series } = merakiTrafficSeries(
      [{ row: 1, name: 'WAN1', sent: pts([120, 3]), recv: pts([60, 0], [180, 4]) }],
      PAL,
      L,
    );
    expect(timestamps).toEqual([60, 120, 180]);
    expect(series.map((s) => s.values)).toEqual([
      [null, 3, null],
      [-0, null, -4],
    ]);
  });

  it('keeps an uplink on its colour when the one before it has no history', () => {
    const { series } = merakiTrafficSeries(
      [
        { row: 1, name: 'WAN1', sent: [], recv: [] },
        { row: 3, name: 'cellular', sent: pts([60, 1]), recv: [] },
      ],
      PAL,
      L,
    );
    expect(series).toEqual([{ label: 'cellular sent', values: [1], color: 'c1' }]);
  });

  it('has nothing to draw when nothing was stored', () => {
    expect(merakiTrafficSeries([{ row: 1, name: 'WAN1', sent: [], recv: [] }], PAL, L)).toEqual({
      timestamps: [],
      series: [],
    });
  });
});

describe('merakiRadioLines', () => {
  const names = new Map([
    [1, '2.4 GHz'],
    [2, '5 GHz'],
  ]);

  // ADR-168: a Meraki access point's radios, one line each in slot order, named by their rows.
  it('joins the utilization and its non-Wi-Fi part by radio, in slot order', () => {
    expect(
      merakiRadioLines([row(2, 3.08), row(1, 37.25)], [row(1, 0.5), row(2, 0)], names),
    ).toEqual([
      { row: 1, band: '2.4 GHz', utilPct: 37.25, nonWifiPct: 0.5 },
      { row: 2, band: '5 GHz', utilPct: 3.08, nonWifiPct: 0 },
    ]);
  });

  it('keeps a radio with one reading, and names a row it has no name for by its key', () => {
    expect(merakiRadioLines([row(12, 9)], [row(2, 1)], names)).toEqual([
      { row: 2, band: '5 GHz', utilPct: null, nonWifiPct: 1 },
      { row: 12, band: '#12', utilPct: 9, nonWifiPct: null },
    ]);
  });

  it('draws nothing for an access point with no radio measured', () => {
    expect(merakiRadioLines([], [], names)).toEqual([]);
  });
});
