// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { InterfaceRow } from '../../types/api';
import { portChoices } from './portChoice';

const row = (over: Partial<InterfaceRow>): InterfaceRow => ({
  ifindex: 1,
  addresses: [],
  stale: false,
  ...over,
});

describe('portChoices', () => {
  it('names a port the way the Interfaces dock does', () => {
    const [named, bare] = portChoices([row({ ifindex: 3, if_name: 'GE0/0/1' }), row({ ifindex: 9 })]);
    expect(named.label).toBe('GE0/0/1');
    expect(bare.label).toBe('if9');
  });

  it('adds the alias to the option, not to the label', () => {
    const [p] = portChoices([row({ if_name: 'GE0/0/1', if_alias: ' uplink ' })]);
    expect(p.label).toBe('GE0/0/1');
    expect(p.option).toBe('GE0/0/1 — uplink');
  });

  it('orders by ifIndex and leaves out ports the device no longer reports', () => {
    const got = portChoices([
      row({ ifindex: 20 }),
      row({ ifindex: 5, stale: true }),
      row({ ifindex: 2 }),
    ]).map((p) => p.ifindex);
    expect(got).toEqual([2, 20]);
  });

  it('carries the declared speed for the port-rule dialog', () => {
    expect(portChoices([row({ if_speed_bps: 1e9 })])[0].speedBps).toBe(1e9);
    expect(portChoices([row({})])[0].speedBps).toBeNull();
  });
});
