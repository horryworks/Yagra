// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { NodeTag } from '../types/api';
import { placesNamed, planAdd, planRename } from './tagCatalog';

const row = (tag: string, over: Partial<NodeTag> = {}): NodeTag => ({
  tag,
  nodes: 0,
  own: 0,
  folders: 0,
  refusals: 0,
  rules: 0,
  exceptions: 0,
  registered: false,
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
  it('counts a registration as one place', () => {
    expect(placesNamed(row('x', { registered: true }))).toBe(1);
  });

  it('adds every place, not the effective node count', () => {
    expect(placesNamed(row('x', { nodes: 9, own: 1, folders: 2, refusals: 1, rules: 3, exceptions: 1 }))).toBe(8);
  });
});

describe('planAdd', () => {
  it('accepts a new name and finds nothing listed', () => {
    expect(planAdd(' TeamC ', tags)).toEqual({ problem: null, listed: null });
  });

  it('names the row a listed name already has', () => {
    expect(planAdd('core', tags)).toEqual({ problem: null, listed: tags[0] });
  });

  it('applies the label rules', () => {
    expect(planAdd('  ', tags).problem).toBe('empty');
    expect(planAdd('x'.repeat(65), tags).problem).toBe('tooLong');
  });
});
