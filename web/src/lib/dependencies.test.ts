import { describe, it, expect } from 'vitest';
import { descendantIds, invalidParentIds } from './dependencies';
import type { TopologyNode } from '../types/api';

/** Minimal topology node — only id/parent_id matter for these graph helpers. */
const n = (id: string, parent: string | null): TopologyNode => ({
  id,
  name: id,
  parent_id: parent,
  state: 'ok',
  root_cause: null,
});

describe('descendantIds', () => {
  it('collects transitive children and excludes the root itself', () => {
    // a → b → c, and a → d (edges point child.parent = upstream).
    const nodes = [n('a', null), n('b', 'a'), n('c', 'b'), n('d', 'a')];
    expect(descendantIds(nodes, 'a')).toEqual(new Set(['b', 'c', 'd']));
    expect(descendantIds(nodes, 'b')).toEqual(new Set(['c']));
    expect(descendantIds(nodes, 'c')).toEqual(new Set());
  });

  it('is cycle-safe (terminates on malformed data)', () => {
    // a ⇄ b cycle; must not loop forever and must not include the root.
    const nodes = [n('a', 'b'), n('b', 'a')];
    expect(descendantIds(nodes, 'a')).toEqual(new Set(['b']));
  });
});

describe('invalidParentIds', () => {
  it('excludes the node itself plus every descendant', () => {
    const nodes = [n('a', null), n('b', 'a'), n('c', 'b')];
    // Offering a, b, or c as a's upstream would create a cycle; d (unrelated) stays valid.
    expect(invalidParentIds(nodes, 'a')).toEqual(new Set(['a', 'b', 'c']));
    expect(invalidParentIds(nodes, 'c')).toEqual(new Set(['c']));
  });
});
