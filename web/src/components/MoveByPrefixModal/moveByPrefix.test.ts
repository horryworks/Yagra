// SPDX-License-Identifier: AGPL-3.0-only
// Reading an IP-range proposal (ADR-124). The first describe is the one that matters: different
// situations that all show zero rows, and telling them apart is the whole point.
import { describe, expect, it } from 'vitest';
import {
  EMPTY_REASONS,
  byDestination,
  emptyReason,
  fromSelection,
  fromSubtree,
  remainingAfterApply,
  summarize,
  type ProposalView,
} from './moveByPrefix';
import type { MovePreview, SubtreeMovePreview } from '../../types/api';
import en from '../../locales/en/nodes.json';
import ja from '../../locales/ja/nodes.json';

const preview = (over: Partial<MovePreview> = {}): ProposalView =>
  fromSelection({
    matched: [],
    ambiguous: [],
    unmatched: [],
    in_place: [],
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

  it('says "already in place" only when nothing else is left over', () => {
    // ADR-176 決定 2: the good kind of zero. With an unmatched node beside it, the operator still
    // has something to look at, so it is not the whole story.
    expect(emptyReason(preview({ in_place: ['a', 'b'] }))).toBe('allInPlace');
    expect(emptyReason(preview({ in_place: ['a'], unmatched: ['b'] }))).toBe('noMatch');
  });

  it('prefers "no ranges" over every other reading', () => {
    // any_prefixes false makes the others impossible, so it is checked first.
    expect(
      emptyReason(
        preview({ ambiguous: [{ node_id: 'a', group_ids: ['g1', 'g2'] }], any_prefixes: false }),
      ),
    ).toBe('noPrefixes');
  });

  it('has a sentence for every reason in both languages', () => {
    for (const r of EMPTY_REASONS) {
      expect(en.moveByPrefix.empty[r], `en ${r}`).toBeTruthy();
      expect(ja.moveByPrefix.empty[r], `ja ${r}`).toBeTruthy();
    }
  });
});

describe('a subtree proposal', () => {
  const subtree = (over: Partial<SubtreeMovePreview> = {}): SubtreeMovePreview => ({
    matched: [],
    matched_total: 0,
    ambiguous: [],
    ambiguous_total: 0,
    unmatched: [],
    unmatched_total: 0,
    in_place_total: 0,
    nodes: [],
    any_prefixes: true,
    ...over,
  });

  it('reads its totals, not the length of its sliced lists', () => {
    // The server lists at most 200 unmatched; the count must still be the real one.
    const view = fromSubtree(subtree({ unmatched: ['a'], unmatched_total: 5000 }));
    expect(view.unmatchedTotal).toBe(5000);
    expect(emptyReason(view)).toBe('noMatch');
  });

  it('leaves the rest of a capped proposal for the next round', () => {
    // ADR-176 決定 4: 1,000 go now, 234 wait for "continue".
    const matched = Array.from({ length: 1000 }, (_, i) => hit(`n${i}`, 'g1'));
    expect(remainingAfterApply(fromSubtree(subtree({ matched, matched_total: 1234 })))).toBe(234);
    expect(remainingAfterApply(fromSubtree(subtree({ matched, matched_total: 1000 })))).toBe(0);
  });

  it('is not empty while proposals remain past the slice', () => {
    expect(emptyReason(fromSubtree(subtree({ matched_total: 3 })))).toBeNull();
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
  it('adds up what landed across every destination', () => {
    const s = summarize([
      { moved: 3, requested: 3 },
      { moved: 1, requested: 2 },
    ]);
    expect(s).toEqual({ moved: 4, requested: 5, complete: false });
  });

  it('reports complete only when everything asked for landed', () => {
    expect(summarize([{ moved: 2, requested: 2 }]).complete).toBe(true);
    // A node deleted between the preview and the apply: the request succeeded, one row did not
    // move, and saying "done" would be a claim nobody checked.
    expect(summarize([{ moved: 1, requested: 2 }]).complete).toBe(false);
  });

  it('is not complete when there was nothing to do', () => {
    expect(summarize([]).complete).toBe(false);
  });
});
