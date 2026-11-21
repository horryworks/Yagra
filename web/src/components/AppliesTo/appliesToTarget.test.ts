// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  JOINS,
  appliesToBody,
  appliesToFrom,
  folderChoices,
  joinMatters,
  namesNothing,
  reachDetail,
  shownNames,
} from './appliesToTarget';

describe('appliesTo', () => {
  it('reads a stored pair with nothing in it as every node, and anything in it as only these', () => {
    expect(appliesToFrom([], []).only).toBe(false);
    expect(appliesToFrom(['g'], []).only).toBe(true);
    expect(appliesToFrom([], ['core']).only).toBe(true);
  });

  it('sends nothing while every node is chosen, keeping the ticks for a switch back', () => {
    const v = { only: false, groupIds: ['g'], tags: ['core'], join: 'and' as const };
    expect(appliesToBody(v)).toEqual({ group_ids: [], tags: [], join: 'or' });
    expect(appliesToBody({ ...v, only: true })).toEqual({
      group_ids: ['g'],
      tags: ['core'],
      join: 'and',
    });
  });

  it('calls "only these" with nothing ticked a choice of nothing', () => {
    expect(namesNothing({ only: true, groupIds: [], tags: [], join: 'or' })).toBe(true);
    expect(namesNothing({ only: false, groupIds: [], tags: [], join: 'or' })).toBe(false);
    expect(namesNothing({ only: true, groupIds: [], tags: ['x'], join: 'or' })).toBe(false);
  });

  it('reads a stored setting with no join as or, the way every rule before the switch reads', () => {
    expect(appliesToFrom(['g'], ['core']).join).toBe('or');
    expect(appliesToFrom(['g'], ['core'], 'and').join).toBe('and');
    expect(JOINS).toEqual(['or', 'and']);
  });

  it('offers or / and only when both lists name something', () => {
    const v = appliesToFrom(['g'], ['core']);
    expect(joinMatters(v)).toBe(true);
    expect(joinMatters({ ...v, tags: [] })).toBe(false);
    expect(joinMatters({ ...v, groupIds: [] })).toBe(false);
    expect(joinMatters({ ...v, only: false })).toBe(false);
  });

  it('shows the first two names and counts the rest', () => {
    expect(shownNames(['a'])).toEqual({ shown: ['a'], more: 0 });
    expect(shownNames(['a', 'b', 'c', 'd'])).toEqual({ shown: ['a', 'b'], more: 2 });
  });

  it('explains the reach by both counts under or, by the folder count under and', () => {
    const parts = { by_folder: 6, by_tag: 7, both: 5 };
    expect(reachDetail(parts, 'or')).toEqual({
      key: 'appliesTo.partsOr',
      values: { folders: 6, tags: 7, both: 5 },
    });
    expect(reachDetail(parts, 'and')).toEqual({
      key: 'appliesTo.partsAnd',
      values: { folders: 6 },
    });
  });

  it('lists a named folder that no longer exists first, then every folder by path', () => {
    const options = [
      { id: 'a', label: 'Tokyo', depth: 0, path: 'Tokyo' },
      { id: 'b', label: 'Edge', depth: 1, path: 'Tokyo / Edge' },
    ];
    expect(folderChoices(options, ['b', 'gone'])).toEqual([
      { id: 'gone', label: 'gone', gone: true },
      { id: 'a', label: 'Tokyo', gone: false },
      { id: 'b', label: 'Tokyo / Edge', gone: false },
    ]);
  });
});
