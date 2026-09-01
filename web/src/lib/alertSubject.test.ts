// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { alertRowKey, alertSubject, rootCause, subjectNodeId } from './alertSubject';

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

describe('rootCause', () => {
  const node = '11111111-1111-4111-8111-111111111111';
  const other = '22222222-2222-4222-8222-222222222222';

  it('reports no cause when the alert carries none', () => {
    expect(rootCause({ node, subject_kind: 'node' }).kind).toBe('none');
  });

  // The case ADR-087 introduced, and the reason this function exists: the arrow every other case
  // renders would say `X ← X`.
  it('distinguishes an alert rolled into its own node from one rolled up under another', () => {
    expect(rootCause({ node, subject_kind: 'node', root_cause: node })).toEqual({ kind: 'self' });
    expect(rootCause({ node, subject_kind: 'node', root_cause: other })).toEqual({
      kind: 'upstream',
      nodeId: other,
    });
  });

  // A pool alert has no node, so it can never be `self` — and reading `node` without the kind is
  // exactly the mistake this module exists to prevent.
  it('never calls a pool alert self-caused', () => {
    expect(
      rootCause({ node: 'pool:tokyo', subject_kind: 'pool', subject_name: 'tokyo', root_cause: other }),
    ).toEqual({ kind: 'upstream', nodeId: other });
  });
});

describe('alertRowKey', () => {
  it('separates the same node’s different checks, and one check’s two severities', () => {
    // Both cases are live: a node alerts on several checks at once, and a check is briefly present
    // at two severities while it escalates. A key that collapsed either would make React reuse one
    // row's DOM for another alert.
    const a = { node: 'n1', check: 'icmp', severity: 'warning' };
    expect(alertRowKey(a)).toBe('n1|icmp|warning');
    expect(alertRowKey({ ...a, check: 'snmp' })).not.toBe(alertRowKey(a));
    expect(alertRowKey({ ...a, severity: 'critical' })).not.toBe(alertRowKey(a));
    expect(alertRowKey({ ...a, node: 'n2' })).not.toBe(alertRowKey(a));
  });
});
