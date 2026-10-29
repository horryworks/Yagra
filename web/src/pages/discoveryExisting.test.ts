// SPDX-License-Identifier: AGPL-3.0-only
// Reading the server's "already in the tree" answer on the Discovery screen (ADR-139).

import { describe, expect, it } from 'vitest';
import type { InventoryMatch, SameDeviceMatch } from '../types/api';
import {
  existingByAddress,
  importableCandidates,
  isImportable,
  sameDeviceBadgeKey,
  sameDeviceByAddress,
  sameDeviceConfirmKey,
  sameDeviceLevel,
  sameDeviceReasonKey,
  sameDeviceReasonKeys,
  sameDeviceTone,
  selectedForImport,
} from './discoveryExisting';

const visible = (address: string): InventoryMatch => ({
  address,
  nodes: [{ id: '00000000-0000-0000-0000-00000000000a', name: 'core-sw01' }],
  outside_scope: false,
});

const hidden = (address: string): InventoryMatch => ({
  address,
  nodes: [],
  outside_scope: true,
});

const cand = (address: string) => ({ address });

describe('existing devices', () => {
  it('keys the answer by the candidate address and treats a missing list as nothing marked', () => {
    const map = existingByAddress([visible('10.0.0.1'), hidden('10.0.0.2')]);
    expect([...map.keys()]).toEqual(['10.0.0.1', '10.0.0.2']);
    // An older core sends no list. The server still refuses a taken address; the screen just
    // cannot say so in advance.
    expect(existingByAddress(undefined).size).toBe(0);
  });

  it('refuses a device node the caller can see and one they cannot, and offers the rest', () => {
    const map = existingByAddress([visible('10.0.0.1'), hidden('10.0.0.2')]);
    expect(isImportable('10.0.0.1', map)).toBe(false);
    expect(isImportable('10.0.0.2', map)).toBe(false);
    expect(isImportable('10.0.0.3', map)).toBe(true);
  });

  it('previews only what can still be imported', () => {
    const map = existingByAddress([visible('10.0.0.1')]);
    expect(importableCandidates([cand('10.0.0.1'), cand('10.0.0.3')], map)).toEqual([
      cand('10.0.0.3'),
    ]);
  });

  it('sends the ticked rows that are not in the tree, in candidate order', () => {
    const map = existingByAddress([visible('10.0.0.2')]);
    const rows = {
      '10.0.0.3': { selected: true },
      // Ticked between two polls, then marked by the server: the tick must not be sent.
      '10.0.0.2': { selected: true },
      '10.0.0.1': { selected: true },
      '10.0.0.4': { selected: false },
    };
    const candidates = [cand('10.0.0.1'), cand('10.0.0.2'), cand('10.0.0.3'), cand('10.0.0.4'), cand('10.0.0.5')];
    expect(selectedForImport(candidates, rows, map)).toEqual([cand('10.0.0.1'), cand('10.0.0.3')]);
  });
});

describe('the same device at another address (ADR-139 increment 3)', () => {
  const node = (confidence: 'exact' | 'confident' | 'possible') => ({
    id: '00000000-0000-0000-0000-00000000000b',
    name: 'core-1',
    address: '192.0.2.1',
    confidence,
    evidence: ['own_ip_one_way' as const],
  });
  const marked = (address: string, ...conf: ('exact' | 'confident' | 'possible')[]): SameDeviceMatch => ({
    address,
    nodes: conf.map(node),
  });

  it('keys the answer by the candidate address and treats a missing list as nothing marked', () => {
    const map = sameDeviceByAddress([marked('198.51.100.1', 'possible')]);
    expect([...map.keys()]).toEqual(['198.51.100.1']);
    expect(sameDeviceByAddress(undefined).size).toBe(0);
  });

  it('leaves a marked row importable — the mark is not a refusal', () => {
    const existing = existingByAddress([]);
    expect(isImportable('198.51.100.1', existing)).toBe(true);
    expect(
      selectedForImport([cand('198.51.100.1')], { '198.51.100.1': { selected: true } }, existing),
    ).toEqual([cand('198.51.100.1')]);
  });

  it('badges the row by its most convincing node', () => {
    expect(sameDeviceBadgeKey(marked('a', 'possible'))).toBe('discovery.sameDevice.maybe');
    expect(sameDeviceBadgeKey(marked('a', 'possible', 'confident'))).toBe(
      'discovery.sameDevice.likely',
    );
  });

  it('names a sentence for each kind the server judges a candidate by, and none for the rest', () => {
    expect(sameDeviceReasonKey('own_ip_one_way')).toBe('discovery.sameDevice.reason.ownIp');
    expect(sameDeviceReasonKey('name')).toBe('discovery.sameDevice.reason.name');
    expect(sameDeviceReasonKey('serial')).toBe('discovery.sameDevice.reason.serial');
    expect(sameDeviceReasonKey('address')).toBeNull();
  });

  it('draws and confirms each level from one table, the strongest node deciding (ADR-139 Inc.5)', () => {
    const exact = marked('a', 'possible', 'exact', 'confident');
    expect(sameDeviceLevel(exact)).toBe('exact');
    expect(sameDeviceBadgeKey(exact)).toBe('discovery.sameDevice.exact');
    expect(sameDeviceTone(exact)).toBe('same-exact');
    expect(sameDeviceConfirmKey(exact)).toBe('discovery.sameDevice.confirm.exact');

    const likely = marked('a', 'possible', 'confident');
    expect(sameDeviceTone(likely)).toBe('same-likely');
    expect(sameDeviceConfirmKey(likely)).toBe('discovery.sameDevice.confirm.likely');

    const maybe = marked('a', 'possible');
    expect(sameDeviceTone(maybe)).toBe('same-maybe');
    expect(sameDeviceConfirmKey(maybe)).toBe('discovery.sameDevice.confirm.maybe');
  });

  it('reads a level from a newer core as maybe', () => {
    const future = {
      address: 'a',
      nodes: [{ ...node('confident'), confidence: 'certain' as unknown as 'possible' }],
    };
    expect(sameDeviceLevel(future)).toBe('possible');
  });

  it('lists each reason once, in the order the nodes give them', () => {
    const m: SameDeviceMatch = {
      address: 'a',
      nodes: [
        { ...node('exact'), evidence: ['serial'] },
        { ...node('confident'), evidence: ['own_ip_one_way', 'name'] },
        { ...node('possible'), evidence: ['name'] },
      ],
    };
    expect(sameDeviceReasonKeys(m)).toEqual([
      'discovery.sameDevice.reason.serial',
      'discovery.sameDevice.reason.ownIp',
      'discovery.sameDevice.reason.name',
    ]);
  });

  it('names nothing for a kind a newer core added — never the raw token as a key', () => {
    // An open tab on an older bundle receives whatever the upgraded core sends (N-1).
    const future = 'a_kind_from_a_newer_core' as unknown as Parameters<typeof sameDeviceReasonKey>[0];
    expect(sameDeviceReasonKey(future)).toBeNull();
  });
});
