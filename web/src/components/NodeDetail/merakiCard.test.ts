// SPDX-License-Identifier: AGPL-3.0-only
// The Overview's Cisco Meraki card (ADR-164 増分 13): what it draws per WAN uplink.

import { describe, expect, it } from 'vitest';
import { merakiUplinkLines } from './merakiCard';

const row = (r: number, value: number, name?: string) => ({ row: r, value, name });

describe('merakiUplinkLines', () => {
  it('joins send and receive by uplink, in row order, with the names the readings carry', () => {
    expect(
      merakiUplinkLines(
        [row(2, 10, 'WAN2'), row(1, 5_000, 'WAN1')],
        [row(1, 10_000, 'WAN1'), row(2, 20, 'WAN2')],
      ),
    ).toEqual([
      { row: 1, name: 'WAN1', sentBps: 5_000, recvBps: 10_000 },
      { row: 2, name: 'WAN2', sentBps: 10, recvBps: 20 },
    ]);
  });

  it('keeps an uplink that reported only one direction, without shifting its neighbour', () => {
    expect(merakiUplinkLines([row(1, 1, 'WAN1'), row(3, 3, 'cellular')], [row(3, 30, 'cellular')])).toEqual([
      { row: 1, name: 'WAN1', sentBps: 1, recvBps: null },
      { row: 3, name: 'cellular', sentBps: 3, recvBps: 30 },
    ]);
  });

  it('keeps an idle uplink: zero is a reading', () => {
    expect(merakiUplinkLines([row(2, 0, 'WAN2')], [row(2, 0, 'WAN2')])).toEqual([
      { row: 2, name: 'WAN2', sentBps: 0, recvBps: 0 },
    ]);
  });

  it('calls a row nobody named by its key, and takes a name from either direction', () => {
    expect(merakiUplinkLines([row(4, 1)], [])).toEqual([{ row: 4, name: '#4', sentBps: 1, recvBps: null }]);
    expect(merakiUplinkLines([row(1, 1)], [row(1, 2, 'WAN1')])[0].name).toBe('WAN1');
  });

  it('draws nothing when no uplink reported', () => {
    expect(merakiUplinkLines([], [])).toEqual([]);
  });
});
