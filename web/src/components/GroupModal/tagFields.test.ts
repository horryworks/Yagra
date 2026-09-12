import { describe, it, expect } from 'vitest';
import { tagDraftFrom, tagsChanged } from './tagFields';
import type { NodeGroup } from '../../types/api';

// The folder dialog's label draft (ADR-135 inc. 2). The same shape `geoFields.test.ts` and
// `prefixFields.test.ts` take: what is worth testing is the changed-check, because the labels are a
// sub-resource saved by a second request that must not fire when nothing moved.

function group(over: Partial<NodeGroup> = {}): NodeGroup {
  return {
    id: 'g1',
    name: 'Tokyo',
    group_type: 'site',
    parent_id: null,
    sort_order: 0,
    latitude: null,
    longitude: null,
    effective_latitude: null,
    effective_longitude: null,
    geo_source: 'unset',
    geo_group: null,
    pool: null,
    prefixes: [],
    tags: [],
    tags_excluded: [],
    effective_tags: [],
    ...over,
  } as NodeGroup;
}

describe('tagDraftFrom', () => {
  it('sorts, so the chips do not reshuffle between two openings of the same dialog', () => {
    const d = tagDraftFrom(group({ tags: ['core', 'JAPAN'], tags_excluded: ['b', 'a'] }));
    expect(d.tags).toEqual(['core', 'JAPAN'].sort((a, b) => a.localeCompare(b)));
    expect(d.tagsExcluded).toEqual(['a', 'b']);
  });

  it('gives a folder being created two empty lists rather than undefined', () => {
    expect(tagDraftFrom(undefined)).toEqual({ tags: [], tagsExcluded: [] });
  });

  it('copies rather than aliasing, so editing the draft cannot mutate the loaded folder', () => {
    const g = group({ tags: ['JAPAN'] });
    const d = tagDraftFrom(g);
    d.tags.push('core');
    expect(g.tags).toEqual(['JAPAN']);
  });
});

describe('tagsChanged', () => {
  it('is false for an untouched dialog, so no second request is issued', () => {
    const g = group({ tags: ['JAPAN'], tags_excluded: ['x'] });
    expect(tagsChanged(tagDraftFrom(g), g)).toBe(false);
  });

  it('is false for a folder being created with nothing typed', () => {
    expect(tagsChanged(tagDraftFrom(undefined), undefined)).toBe(false);
  });

  it('sees an addition, a removal, and a change to the refusals', () => {
    const g = group({ tags: ['JAPAN'], tags_excluded: [] });
    expect(tagsChanged({ tags: ['JAPAN', 'core'], tagsExcluded: [] }, g)).toBe(true);
    expect(tagsChanged({ tags: [], tagsExcluded: [] }, g)).toBe(true);
    expect(tagsChanged({ tags: ['JAPAN'], tagsExcluded: ['x'] }, g)).toBe(true);
  });

  it('ignores order, because the chip input appends and the draft is loaded sorted', () => {
    // ⚠️ A positional comparison would call "removed a label and typed it back" a change, and put
    // an audit row in for a write that alters nothing — every time somebody opened the dialog and
    // fiddled.
    const g = group({ tags: ['JAPAN', 'core'] });
    expect(tagsChanged({ tags: ['core', 'JAPAN'], tagsExcluded: [] }, g)).toBe(false);
  });
});
