// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import i18n from '../i18n';
import { defaultAnalysisInput, filterNodes, groupScopeLabel, nodeScopeLabel } from './scope';
import type { NodeSummary } from '../types/api';

// The label builders take the caller's `t`; the global instance (English bundled, default lng) is
// already a standalone bound translator, so pass it straight through for the pure-function tests.
const t = i18n.t;

function node(over: Partial<NodeSummary>): NodeSummary {
  return {
    id: 'n',
    name: 'node',
    address: '10.0.0.1',
    state: 'ok',
    vendor: null,
    model: null,
    group_id: null,
    sort_order: 0,
    source: 'device',
    ...over,
  };
}

describe('scope helpers', () => {
  const nodes = [
    node({ id: 'a', name: 'edge-tok-fw01', address: '10.1.0.1' }),
    node({ id: 'b', name: 'core-mat-rt01', address: '10.2.0.5' }),
    node({ id: 'c', name: 'dist-osk-sw03', address: '192.168.1.7' }),
  ];

  it('returns the full list for an empty query', () => {
    expect(filterNodes(nodes, '   ')).toHaveLength(3);
  });

  it('matches on name (case-insensitive)', () => {
    expect(filterNodes(nodes, 'TOK').map((n) => n.id)).toEqual(['a']);
    expect(filterNodes(nodes, 'rt').map((n) => n.id)).toEqual(['b']);
  });

  it('matches on IP address', () => {
    expect(filterNodes(nodes, '192.168').map((n) => n.id)).toEqual(['c']);
    expect(filterNodes(nodes, '10.').map((n) => n.id)).toEqual(['a', 'b']);
  });

  it('returns nothing when no node matches', () => {
    expect(filterNodes(nodes, 'zzz')).toHaveLength(0);
  });

  it('builds scope labels (group is recursive, node is single)', () => {
    expect(groupScopeLabel('Tokyo', t)).toBe('group: Tokyo (incl. subgroups)');
    expect(nodeScopeLabel('edge-tok-fw01', t)).toBe('node: edge-tok-fw01');
  });

  it('quick-run default input targets all nodes with standard defaults', () => {
    const input = defaultAnalysisInput('anomaly', t);
    expect(input.tool).toBe('anomaly');
    expect(input.scope_kind).toBe('all');
    expect(input.scope_id).toBeNull();
    expect(input.depth).toBe('standard');
    expect(input.window_secs).toBe(7 * 86_400);
  });
});
