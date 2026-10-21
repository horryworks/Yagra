// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { NodeGroup } from '../../types/api';
import { nodeHeaderActions, type NodeHeaderCaps } from './nodeHeaderActions';

const all: NodeHeaderCaps = {
  canConfig: true,
  canMaintenance: true,
  canMute: true,
  pinsReady: true,
  inline: true,
};
const none: NodeHeaderCaps = {
  canConfig: false,
  canMaintenance: false,
  canMute: false,
  pinsReady: false,
  inline: false,
};
const device = { kind: 'device' as const, name: 'sw-01', sys_name: 'sw-01' };
const ranged = [{ id: 'g', prefixes: ['192.0.2.0/24'] } as unknown as NodeGroup];

describe('nodeHeaderActions', () => {
  it('draws the four buttons and the whole menu for an account that may do everything', () => {
    const got = nodeHeaderActions(all, { ...device, sys_name: 'core-sw' }, ranged);
    expect(got.primary).toEqual(['edit', 'maintenance', 'mute', 'pollNow']);
    expect(got.more).toEqual([
      'openDetail',
      'map',
      'pin',
      'upstream',
      'move',
      'moveByPrefix',
      'pool',
      'rediscover',
      'nameFromDevice',
      'delete',
    ]);
  });

  it('draws nothing a viewer cannot use, and keeps the map', () => {
    const got = nodeHeaderActions(none, device, ranged);
    expect(got.primary).toEqual([]);
    expect(got.more).toEqual(['map']);
  });

  it('gates maintenance and mute on their own permissions, not on editing', () => {
    const got = nodeHeaderActions({ ...none, canMute: true }, device, []);
    expect(got.primary).toEqual(['mute']);
    expect(nodeHeaderActions({ ...none, canMaintenance: true }, device, []).primary).toEqual([
      'maintenance',
    ]);
  });

  it('gives the full page the same set as the pane, less the way to open itself', () => {
    const pane = nodeHeaderActions(all, device, ranged);
    const page = nodeHeaderActions({ ...all, inline: false }, device, ranged);
    expect(page.primary).toEqual(pane.primary);
    expect(page.more).toEqual(pane.more.filter((k) => k !== 'openDetail'));
  });

  it('offers Rediscover and the device name only for a device', () => {
    const url = nodeHeaderActions(all, { kind: 'url', name: 'x', sys_name: 'y' }, ranged).more;
    expect(url).not.toContain('rediscover');
    expect(url).not.toContain('nameFromDevice');
  });

  it('offers the device name only when it differs from the node name', () => {
    expect(nodeHeaderActions(all, device, ranged).more).not.toContain('nameFromDevice');
    expect(nodeHeaderActions(all, { ...device, sys_name: null }, ranged).more).not.toContain(
      'nameFromDevice',
    );
  });

  it('offers a move by IP range only when some group has one', () => {
    expect(nodeHeaderActions(all, device, []).more).not.toContain('moveByPrefix');
  });
});
