// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { NodeTag } from '../types/api';
import { placesNamed, planRename } from './tagCatalog';

const row = (tag: string, over: Partial<NodeTag> = {}): NodeTag => ({
  tag,
  nodes: 0,
  own: 0,
  folders: 0,
  refusals: 0,
  rules: 0,
  exceptions: 0,
  ...over,
});

const tags = [row('core', { nodes: 4, own: 4 }), row('TeamA', { nodes: 2, folders: 1 })];

describe('planRename', () => {
  it('accepts a new name and says nothing merges', () => {
    expect(planRename('core', 'backbone', tags)).toEqual({ problem: null, mergeInto: null });
  });

  it('names the existing tag a rename merges into', () => {
    expect(planRename('core', ' TeamA ', tags)).toEqual({ problem: null, mergeInto: tags[1] });
  });

  it('refuses the same name, but a change of case is a rename', () => {
    expect(planRename('core', ' core ', tags).problem).toBe('same');
    expect(planRename('core', 'Core', tags)).toEqual({ problem: null, mergeInto: null });
  });

  it('applies the label rules to the new name', () => {
    expect(planRename('core', '   ', tags).problem).toBe('empty');
    expect(planRename('core', 'x'.repeat(65), tags).problem).toBe('tooLong');
    expect(planRename('core', 'a\tb', tags).problem).toBe('control');
  });

  it('never reports the per-row cap, which a rename cannot cross', () => {
    expect(planRename('core', 'fresh', tags).problem).toBeNull();
  });
});

describe('placesNamed', () => {
  it('adds every place, not the effective node count', () => {
    expect(placesNamed(row('x', { nodes: 9, own: 1, folders: 2, refusals: 1, rules: 3, exceptions: 1 }))).toBe(8);
  });
});
