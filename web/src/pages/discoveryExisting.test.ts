// SPDX-License-Identifier: AGPL-3.0-only
// Reading the server's "already in the tree" answer on the Discovery screen (ADR-139).

import { describe, expect, it } from 'vitest';
import type { InventoryMatch } from '../types/api';
import {
  existingByAddress,
  importableCandidates,
  isImportable,
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
