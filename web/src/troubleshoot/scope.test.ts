// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import i18n from '../i18n';
import { defaultAnalysisInput, groupScopeLabel, nodeScopeLabel } from './scope';

// The label builders take the caller's `t`; the global instance (English bundled, default lng) is
// already a standalone bound translator, so pass it straight through for the pure-function tests.
const t = i18n.t;

describe('scope helpers', () => {
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
