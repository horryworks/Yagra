// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import i18n from '../../i18n';
import { allScope, groupScopeLabel, nodeScopeLabel } from './scope';

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
