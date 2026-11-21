// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { appliesToBody, appliesToFrom, folderChoices, namesNothing } from './appliesToTarget';

describe('appliesTo', () => {
  it('reads a stored pair with nothing in it as every node, and anything in it as only these', () => {
    expect(appliesToFrom([], []).only).toBe(false);
    expect(appliesToFrom(['g'], []).only).toBe(true);
    expect(appliesToFrom([], ['core']).only).toBe(true);
  });

  it('sends nothing while every node is chosen, keeping the ticks for a switch back', () => {
    const v = { only: false, groupIds: ['g'], tags: ['core'] };
    expect(appliesToBody(v)).toEqual({ group_ids: [], tags: [] });
    expect(appliesToBody({ ...v, only: true })).toEqual({ group_ids: ['g'], tags: ['core'] });
  });

  it('calls "only these" with nothing ticked a choice of nothing', () => {
    expect(namesNothing({ only: true, groupIds: [], tags: [] })).toBe(true);
    expect(namesNothing({ only: false, groupIds: [], tags: [] })).toBe(false);
    expect(namesNothing({ only: true, groupIds: [], tags: ['x'] })).toBe(false);
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
