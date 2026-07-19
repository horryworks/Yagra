import { describe, it, expect } from 'vitest';
import { NODE_DETAIL_TABS, normalizeNodeDetailTab } from './tabs';

// Regression guard for the ADR-031 Flow-tab bug: the node-detail tab whitelist was duplicated across
// NodeDetail.tsx, NodeDetailPage.tsx, and NodesPage.tsx (the split-view host). The split-view copy
// was missed when Flow was added, so the Flow button rendered but clicking it bounced back to
// Overview. All three now import NODE_DETAIL_TABS, so they cannot drift — these tests pin the shared
// source and the normalization the three surfaces rely on.
describe('node-detail tabs', () => {
  it('whitelists every rendered tab, including flow', () => {
    expect([...NODE_DETAIL_TABS]).toEqual([
      'overview',
      'interfaces',
      'collection',
      'events',
      'flow',
    ]);
    expect(NODE_DETAIL_TABS).toContain('flow');
  });

  it('normalizes a known tab to itself (so the split view keeps it selected)', () => {
    for (const tab of NODE_DETAIL_TABS) {
      expect(normalizeNodeDetailTab(tab)).toBe(tab);
    }
  });

  it('falls back to overview for unknown, empty, or wrong-case tabs', () => {
    expect(normalizeNodeDetailTab('')).toBe('overview');
    expect(normalizeNodeDetailTab('bogus')).toBe('overview');
    expect(normalizeNodeDetailTab('FLOW')).toBe('overview');
  });
});
