// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import i18n from '../../i18n';
import { allScope, groupScopeLabel, nodeScopeLabel, scopeFromUrl, scopeLabelOf } from './scope';

// The label builders take the caller's `t`; the global instance (English bundled, default lng) is
// already a standalone bound translator, so pass it straight through for the pure-function tests.
const t = i18n.t;

describe('scope labels', () => {
  it('distinguishes a recursive group scope from a single node', () => {
    // The "(incl. subgroups)" is not decoration — a group scope covers its subtree (ADR-022), and
    // an operator reading "group: Tokyo" would reasonably expect only Tokyo's direct members.
    expect(groupScopeLabel('Tokyo', t)).toBe('group: Tokyo (incl. subgroups)');
    expect(nodeScopeLabel('edge-tok-fw01', t)).toBe('node: edge-tok-fw01');
  });

  it('defaults to everything, labelled', () => {
    expect(allScope(t)).toEqual({ kind: 'all', id: null, label: 'All nodes' });
  });

  it('resolves in the shared namespace, not the one it was written in', () => {
    // The keys moved from `troubleshoot` to `common` when the picker moved out of that domain. A
    // key that fails to resolve renders as the raw key, and the i18n parity gate cannot catch it:
    // parity compares the two locales, and a key missing from both is missing symmetrically.
    for (const label of [allScope(t).label, groupScopeLabel('x', t), nodeScopeLabel('y', t)]) {
      expect(label).not.toContain('scope.');
    }
  });
});

describe('the scope a URL names', () => {
  const none = { nodeId: '', groupId: '' };
  const picked = { kind: 'group' as const, id: 'g1', label: groupScopeLabel('Tokyo', t) };

  it('keeps the picked scope, with its label, while the URL still names it', () => {
    expect(scopeFromUrl(picked, { nodeId: '', groupId: 'g1' }, t)).toBe(picked);
  });

  it('follows the URL when Back or a deep link names another scope', () => {
    expect(scopeFromUrl(picked, { nodeId: '', groupId: 'g2' }, t)).toEqual({
      kind: 'group',
      id: 'g2',
      label: 'g2',
    });
    expect(scopeFromUrl(picked, { nodeId: 'n1', groupId: '' }, t)).toEqual({
      kind: 'node',
      id: 'n1',
      label: 'n1',
    });
    expect(scopeFromUrl(picked, none, t)).toEqual(allScope(t));
  });

  it('names a bare id once the caller can, and leaves a picked label alone', () => {
    const name = (id: string) => (id === 'g2' ? 'Osaka' : id === 'n1' ? 'sw-01' : id);
    const label = (s: ReturnType<typeof scopeFromUrl>) => scopeLabelOf(s, name, name, t);
    expect(label(scopeFromUrl(picked, { nodeId: '', groupId: 'g2' }, t))).toBe(groupScopeLabel('Osaka', t));
    expect(label(scopeFromUrl(picked, { nodeId: 'n1', groupId: '' }, t))).toBe(nodeScopeLabel('sw-01', t));
    expect(label(picked)).toBe(picked.label);
    expect(label(allScope(t))).toBe(allScope(t).label);
  });
});
