// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  alertNodeIds,
  NODE_NAMES_BATCH,
  nodeNameBatches,
  pinAlertRows,
  placesFromNames,
} from './geoPinAlerts';

type A = { node: string; subject_kind?: 'node' | 'pool' | 'meraki_org'; tag: string };
const alert = (node: string, tag: string): A => ({ node, subject_kind: 'node', tag });

describe('alertNodeIds', () => {
  it('lists each node once and leaves out alerts about a pool or an organization', () => {
    const ids = alertNodeIds([
      alert('n1', 'a'),
      alert('n2', 'b'),
      alert('n1', 'c'),
      { node: 'pool:default', subject_kind: 'pool', tag: 'p' },
      { node: 'meraki_org:7', subject_kind: 'meraki_org', tag: 'm' },
    ]);
    expect(ids).toEqual(['n1', 'n2']);
  });
});

describe('nodeNameBatches', () => {
  it('cuts the ids into requests the endpoint accepts', () => {
    const ids = Array.from({ length: NODE_NAMES_BATCH * 2 + 3 }, (_, i) => `n${i}`);
    const batches = nodeNameBatches(ids);
    expect(batches.map((b) => b.length)).toEqual([NODE_NAMES_BATCH, NODE_NAMES_BATCH, 3]);
    expect(batches.flat()).toEqual(ids);
    expect(nodeNameBatches([])).toEqual([]);
  });
});

describe('placesFromNames', () => {
  it('reads the folder, and null for an ungrouped node', () => {
    const places = placesFromNames([
      { id: 'n1', name: 'sw-01', group_id: 'g1' },
      { id: 'n2', name: 'loose', group_id: null },
    ]);
    expect(places?.get('n1')).toEqual({ name: 'sw-01', group: 'g1' });
    expect(places?.get('n2')).toEqual({ name: 'loose', group: null });
  });

  it('answers null for a core that does not send group_id at all', () => {
    expect(placesFromNames([{ id: 'n1', name: 'sw-01' }])).toBeNull();
  });

  it('treats an empty answer as known: nothing to place', () => {
    expect(placesFromNames([])?.size).toBe(0);
  });
});

describe('pinAlertRows', () => {
  // g-floor inherits its position from g-site, so both resolve to the pin g-site.
  const pinOf = new Map([
    ['g-site', 'g-site'],
    ['g-floor', 'g-site'],
    ['g-other', 'g-other'],
  ]);
  const places = new Map([
    ['n1', { name: 'sw-core', group: 'g-site' }],
    ['n2', { name: 'ap-3f', group: 'g-floor' }],
    ['n3', { name: 'rt-far', group: 'g-other' }],
    ['n4', { name: 'loose', group: null }],
  ]);

  it('keeps one row per node under the pin, worst alert first, with the count', () => {
    // Already worst-first, as `sortedAlerts` hands them over.
    const alerts = [
      alert('n2', 'critical-ap'),
      alert('n1', 'critical-core'),
      alert('n3', 'elsewhere'),
      alert('n2', 'warning-ap'),
      alert('n4', 'ungrouped'),
      alert('n9', 'unknown-node'),
    ];
    const rows = pinAlertRows(alerts, places, pinOf, 'g-site');
    expect(rows.map((r) => [r.name, r.worst.tag, r.count])).toEqual([
      ['ap-3f', 'critical-ap', 2],
      ['sw-core', 'critical-core', 1],
    ]);
  });

  it('answers nothing for a pin whose nodes are all quiet', () => {
    expect(pinAlertRows([alert('n3', 'x')], places, pinOf, 'g-site')).toEqual([]);
  });
});
