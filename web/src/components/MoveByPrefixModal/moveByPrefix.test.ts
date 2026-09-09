// SPDX-License-Identifier: AGPL-3.0-only
// Reading an IP-range proposal (ADR-124). The first describe is the one that matters: three
// different situations that all show zero rows, and telling them apart is the whole point.
import { describe, expect, it } from 'vitest';
import { byDestination, emptyReason, summarize } from './moveByPrefix';
import type { MovePreview } from '../../types/api';

const preview = (over: Partial<MovePreview> = {}): MovePreview => ({
  matched: [],
  ambiguous: [],
  unmatched: [],
  any_prefixes: true,
  ...over,
});

const hit = (node: string, group: string, prefix = '10.0.0.0/24') => ({
  node_id: node,
  group_id: group,
  prefix,
});

describe('emptyReason', () => {
  it('says nothing when there is something to move', () => {
    expect(emptyReason(preview({ matched: [hit('a', 'g1')] }))).toBeNull();
  });

  it('distinguishes "no ranges exist" from "nothing matched"', () => {
    // 🚨 A deployment with no NetBox has no prefixes at all. Reporting every node as unmatched
    // would tell the operator their addresses are wrong, when the truth is that this feature has
    // nothing behind it here.
    expect(emptyReason(preview({ unmatched: ['a'], any_prefixes: false }))).toBe('noPrefixes');
    expect(emptyReason(preview({ unmatched: ['a'] }))).toBe('noMatch');
  });

  it('reports all-ambiguous only when nothing also failed to match', () => {
    expect(emptyReason(preview({ ambiguous: [{ node_id: 'a', group_ids: ['g1', 'g2'] }] }))).toBe(
      'allAmbiguous',
    );
    // A mixture is more honestly "nothing landed on one folder" than "everything was ambiguous".
    expect(
      emptyReason(
        preview({ ambiguous: [{ node_id: 'a', group_ids: ['g1', 'g2'] }], unmatched: ['b'] }),
      ),
    ).toBe('noMatch');
  });

  it('prefers "no ranges" over every other reading', () => {
    // any_prefixes false makes the other two impossible, so it is checked first.
    expect(
      emptyReason(
        preview({ ambiguous: [{ node_id: 'a', group_ids: ['g1', 'g2'] }], any_prefixes: false }),
      ),
    ).toBe('noPrefixes');
  });
});

describe('byDestination', () => {
  it('collects nodes under the folder they would go to, first-seen order', () => {
    const dests = byDestination(
      preview({
        matched: [hit('a', 'g1'), hit('b', 'g2'), hit('c', 'g1')],
      }),
    );
    expect(dests).toEqual([
      { groupId: 'g1', nodeIds: ['a', 'c'] },
      { groupId: 'g2', nodeIds: ['b'] },
    ]);
  });

  it('is empty when nothing is proposed', () => {
    expect(byDestination(preview({ unmatched: ['a'] }))).toEqual([]);
  });

  it('leaves ambiguous nodes out of the plan', () => {
    // They are shown to the operator and never moved: choosing between two sites is the decision
    // this feature refuses to make.
    const dests = byDestination(
      preview({ matched: [hit('a', 'g1')], ambiguous: [{ node_id: 'b', group_ids: ['g2', 'g3'] }] }),
    );
    expect(dests).toEqual([{ groupId: 'g1', nodeIds: ['a'] }]);
  });
});

describe('summarize', () => {
  it('adds up what landed and names the destinations that failed', () => {
    const s = summarize([
      { groupId: 'g1', moved: 3, requested: 3, failed: false },
      { groupId: 'g2', moved: 0, requested: 2, failed: true },
    ]);
    expect(s).toEqual({ moved: 3, requested: 5, failedGroups: ['g2'], complete: false });
  });

  it('reports complete only when everything asked for landed', () => {
    expect(summarize([{ groupId: 'g1', moved: 2, requested: 2, failed: false }]).complete).toBe(
      true,
    );
    // A node deleted between the preview and the apply: the request succeeded, one row did not
    // move, and saying "done" would be a claim nobody checked.
    expect(summarize([{ groupId: 'g1', moved: 1, requested: 2, failed: false }]).complete).toBe(
      false,
    );
  });

  it('is not complete when there was nothing to do', () => {
    expect(summarize([]).complete).toBe(false);
  });
});
