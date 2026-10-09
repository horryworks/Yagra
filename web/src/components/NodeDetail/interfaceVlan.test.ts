import { describe, expect, it } from 'vitest';
import type { InterfaceVlan } from '../../types/api';
import {
  carriedVlanTokens,
  carries,
  effectiveVlan,
  formatSpans,
  memberNames,
  parseVlanId,
  vlanCell,
  vlanCellHasDetail,
  vlanModeKey,
  vlanText,
  type VlanWords,
} from './interfaceVlan';

const base: InterfaceVlan = {
  mode: 'not_l2',
  native: null,
  access_vlan: null,
  voice_vlan: null,
  allowed: [],
  untagged: [],
  tagged: [],
  lag: null,
  members: [],
};
const v = (o: Partial<InterfaceVlan>): InterfaceVlan => ({ ...base, ...o });

const words: VlanWords = {
  native: 'native',
  noNative: 'no native',
  all: 'all',
  untagged: 'untagged',
  tagged: 'tagged',
  voice: 'voice',
  inLag: 'in',
  notL2: 'n/a',
  notReported: 'not reported',
};

// The shapes walked for ADR-201, with made-up names: an Eth-Trunk and its two members, a trunk
// allowing everything, an access port with a voice VLAN, a hybrid port, a routed port.
const trunk = {
  ifindex: 215,
  vlan: v({
    mode: 'trunk',
    native: 1,
    allowed: [
      { first: 700, last: 700 },
      { first: 801, last: 869 },
      { first: 872, last: 889 },
    ],
    members: [
      { ifindex: 55, name: 'XGE0/0/1' },
      { ifindex: 159, name: 'XGE2/0/1' },
    ],
  }),
};
const member = { ifindex: 55, vlan: v({ mode: 'member', lag: { ifindex: 215, name: 'Eth-Trunk0' } }) };
const all = { ifindex: 9, vlan: v({ mode: 'trunk', native: null, allowed: [{ first: 1, last: 4094 }] }) };
const access = { ifindex: 7, vlan: v({ mode: 'access', access_vlan: 100, voice_vlan: 200 }) };
const hybrid = {
  ifindex: 12,
  vlan: v({
    mode: 'hybrid',
    native: 1,
    untagged: [{ first: 1, last: 1 }],
    tagged: [
      { first: 100, last: 100 },
      { first: 120, last: 121 },
    ],
  }),
};
const routed = { ifindex: 40, vlan: v({ mode: 'not_l2' }) };
const byIfindex = new Map([trunk, member, all, access, hybrid, routed].map((r) => [r.ifindex, r]));

describe('the MODE cell', () => {
  it('names each mode, and calls a row with no VLAN facts not reported', () => {
    expect(vlanModeKey(trunk)).toBe('trunk');
    expect(vlanModeKey(member)).toBe('member');
    expect(vlanModeKey(routed)).toBe('not_l2');
    expect(vlanModeKey({ vlan: v({ mode: 'unknown' }) })).toBe('not_reported');
    expect(vlanModeKey({ vlan: null })).toBe('not_reported');
    // A core older than ADR-201 sends no field at all.
    expect(vlanModeKey({})).toBe('not_reported');
  });
});

describe('the VLAN cell', () => {
  it('writes a trunk as its configured native VLAN and its allow list, CLI-style', () => {
    expect(vlanText(vlanCell(trunk), words)).toBe('native 1 · 700,801-869,872-889');
    expect(vlanText(vlanCell(all), words)).toBe('no native · all');
  });

  it('writes the other shapes', () => {
    expect(vlanText(vlanCell(access), words)).toBe('100 + voice 200');
    expect(vlanText(vlanCell(hybrid), words)).toBe('untagged 1 · tagged 100,120-121');
    expect(vlanText(vlanCell(member), words)).toBe('in Eth-Trunk0');
    expect(vlanText(vlanCell(routed), words)).toBe('n/a');
    expect(vlanText(vlanCell({ vlan: null }), words)).toBe('not reported');
  });

  it('lists an aggregate’s members, and nobody else’s', () => {
    expect(memberNames(trunk).map((m) => m.name)).toEqual(['XGE0/0/1', 'XGE2/0/1']);
    expect(memberNames(access)).toEqual([]);
  });

  it('has nothing to add beside the MODE word for a port that does not switch or is not reported', () => {
    expect(vlanCellHasDetail(vlanCell(routed))).toBe(false);
    expect(vlanCellHasDetail(vlanCell({ vlan: v({ mode: 'unknown' }) }))).toBe(false);
    expect(vlanCellHasDetail(vlanCell({ vlan: null }))).toBe(false);
    for (const row of [trunk, member, access, hybrid]) expect(vlanCellHasDetail(vlanCell(row))).toBe(true);
  });

  it('formats single VLANs and ranges', () => {
    expect(formatSpans([{ first: 5, last: 5 }, { first: 7, last: 9 }])).toBe('5,7-9');
  });
});

describe('which VLANs a port carries', () => {
  it('reads a trunk’s native VLAN and its ranges', () => {
    expect(carries(trunk, byIfindex, 1)).toBe(true);
    expect(carries(trunk, byIfindex, 850)).toBe(true);
    expect(carries(trunk, byIfindex, 870)).toBe(false);
  });

  it('answers for a member through its aggregate', () => {
    expect(carries(member, byIfindex, 850)).toBe(true);
    expect(effectiveVlan(member, byIfindex)).toBe(trunk.vlan);
    expect(carries(member, new Map(), 850)).toBe(false);
  });

  it('reads access, voice and hybrid VLANs, and nothing on a routed port', () => {
    expect(carries(access, byIfindex, 200)).toBe(true);
    expect(carries(hybrid, byIfindex, 121)).toBe(true);
    expect(carries(hybrid, byIfindex, 122)).toBe(false);
    expect(carries(routed, byIfindex, 1)).toBe(false);
  });

  it('expands into filter tokens once per VLAN object', () => {
    const tokens = carriedVlanTokens(all.vlan);
    expect(tokens).toHaveLength(4094);
    expect(carriedVlanTokens(all.vlan)).toBe(tokens);
    expect(carriedVlanTokens(trunk.vlan)).toContain('850');
    expect(carriedVlanTokens(null)).toEqual([]);
  });

  it('accepts a VLAN ID in the filter and refuses anything else', () => {
    expect(parseVlanId(' 0850 ')).toBe('850');
    expect(parseVlanId('0')).toBeNull();
    expect(parseVlanId('4095')).toBeNull();
    expect(parseVlanId('10-20')).toBeNull();
  });
});
