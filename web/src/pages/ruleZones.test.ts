// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { zoneLine, zoneQuery } from './ruleZones';

const z = (zone: string, nodes: number, isDefault = false) => ({ zone, nodes, default: isDefault });

describe('ruleZones', () => {
  it('names up to three zones and counts the rest', () => {
    expect(zoneLine([z('Asia/Tokyo', 7)])).toEqual({ shown: [z('Asia/Tokyo', 7)], more: 0 });
    const many = [z('Asia/Tokyo', 5), z('Europe/London', 2), z('UTC', 2, true), z('America/Chicago', 1)];
    expect(zoneLine(many)).toEqual({ shown: many.slice(0, 3), more: 1 });
  });

  it('asks for the rule own folders and tags, or every node when it names none', () => {
    expect(
      zoneQuery({ only: true, groupIds: ['g'], tags: ['core'], join: 'and', tagMatch: 'all' }),
    ).toEqual({
      tags: ['core'],
      groups: ['g'],
      join: 'and',
      tagMatch: 'all',
      everyNode: false,
    });
    for (const v of [
      { only: false, groupIds: ['g'], tags: [], join: 'or' as const, tagMatch: 'any' as const },
      { only: true, groupIds: [], tags: [], join: 'or' as const, tagMatch: 'any' as const },
    ]) {
      expect(zoneQuery(v).everyNode).toBe(true);
    }
  });
});
