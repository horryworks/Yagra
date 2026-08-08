// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { alertSubject, subjectNodeId } from './alertSubject';

const NODE = '6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60';

describe('alertSubject', () => {
  it('reads a node alert as its node', () => {
    const s = alertSubject({ node: NODE, subject_kind: 'node' });
    expect(s).toEqual({ kind: 'node', nodeId: NODE });
    expect(subjectNodeId({ node: NODE, subject_kind: 'node' })).toBe(NODE);
  });

  it('reads a pool alert as its pool name, never as a node', () => {
    // The regression this guards: `node` is `pool:tokyo` on this row, and rendering it through the
    // inventory resolver shows an operator an unresolvable UUID-shaped id instead of their pool.
    const row = { node: 'pool:tokyo', subject_kind: 'pool' as const, subject_name: 'tokyo' };
    expect(alertSubject(row)).toEqual({ kind: 'pool', name: 'tokyo' });
    expect(subjectNodeId(row)).toBeNull();
  });

  it('treats a frame with no subject_kind as a node', () => {
    // N-1: every alert produced before the field existed was a node alert, and one can still be
    // in flight from an older core mid-upgrade.
    expect(alertSubject({ node: NODE })).toEqual({ kind: 'node', nodeId: NODE });
  });

  it('does not invent a name for a pool row that carries none', () => {
    expect(alertSubject({ node: 'pool:x', subject_kind: 'pool' })).toEqual({
      kind: 'pool',
      name: '?',
    });
  });

  it('gates the node actions on a resolvable node, which is what the page checks', () => {
    // Mute names a node and RCA takes a node id, so both fail server-side for a pool subject.
    // `ActiveAlertsPage` gates on `subjectNodeId(a) === null` — it needs the id anyway, so this
    // asserts the predicate the page actually uses rather than a second one alongside it.
    expect(subjectNodeId({ node: NODE, subject_kind: 'node' })).not.toBeNull();
    expect(
      subjectNodeId({ node: 'pool:tokyo', subject_kind: 'pool', subject_name: 'tokyo' }),
    ).toBeNull();
  });
});
