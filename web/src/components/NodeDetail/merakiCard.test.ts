// SPDX-License-Identifier: AGPL-3.0-only
// The Overview's Cisco Meraki card (ADR-164 増分 13): what it draws per WAN uplink.

import { describe, expect, it } from 'vitest';
import { MERAKI_UPLINK_STATES, merakiUplinkLines, merakiUplinkState } from './merakiCard';

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
