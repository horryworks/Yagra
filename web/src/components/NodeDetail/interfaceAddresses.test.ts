// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { addressCellText, addressesOf, formatAddress } from './interfaceAddresses';

describe('formatAddress', () => {
  it('spells an address as ip/prefix', () => {
    expect(formatAddress({ ip: '192.168.0.1', prefix_len: 24 })).toBe('192.168.0.1/24');
    expect(formatAddress({ ip: '10.203.250.42', prefix_len: 30 })).toBe('10.203.250.42/30');
    expect(formatAddress({ ip: '2804:44cc::156:148', prefix_len: 64 })).toBe(
      '2804:44cc::156:148/64',
    );
  });

  it('shows an address whose prefix is unknown bare, never as /0', () => {
    // The PoC recording's `fec0::a:0:0:4` on a Juniper vMX: real address, undecodable prefix.
    expect(formatAddress({ ip: 'fec0::a:0:0:4', prefix_len: null })).toBe('fec0::a:0:0:4');
    expect(formatAddress({ ip: '192.168.178.113' })).toBe('192.168.178.113');
  });
});

describe('addressCellText', () => {
  it('shows the first address and counts the rest, keeping every one for the popover', () => {
    // The recording's Vlanif100 shape, cut to four: the first is the server's first — the cell
    // does not re-sort, because the server's order is the one the popover and the dock share.
    const cell = addressCellText([
      { ip: '10.204.29.254', prefix_len: 24 },
      { ip: '10.221.1.254', prefix_len: 24 },
      { ip: '10.221.2.254', prefix_len: 24 },
      { ip: 'fec0::a:0:0:4', prefix_len: null },
    ]);
    expect(cell).toEqual({
      first: '10.204.29.254/24',
      more: 3,
      all: ['10.204.29.254/24', '10.221.1.254/24', '10.221.2.254/24', 'fec0::a:0:0:4'],
    });
  });

  it('needs no +N for a port with exactly one address', () => {
    expect(addressCellText([{ ip: '10.203.250.42', prefix_len: 30 }])).toEqual({
      first: '10.203.250.42/30',
      more: 0,
      all: ['10.203.250.42/30'],
    });
  });

  it('is null for a port with no address, so the cell draws a dash', () => {
    expect(addressCellText([])).toBeNull();
  });
});

describe('addressesOf', () => {
  it('reads the row field, and treats a core that predates it as a port with none', () => {
    expect(addressesOf({ addresses: [{ ip: '10.0.0.1', prefix_len: 8 }] })).toHaveLength(1);
    expect(addressesOf({ addresses: null })).toEqual([]);
    // An older core's row has no `addresses` key at all (ADR-157 danger list).
    expect(addressesOf({})).toEqual([]);
  });
});
