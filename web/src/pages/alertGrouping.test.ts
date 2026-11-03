import { describe, expect, it } from 'vitest';
import type { AlertGroupingConfig, NodeGroup } from '../types/api';
import { folderLevelRows, isGroupingLevel, notifiedAt } from './alertGrouping';

const cfg = (over: Partial<AlertGroupingConfig> = {}): AlertGroupingConfig => ({
  level: 'off',
  folders: [],
  now: { down: 7, notified: [7, 3, 3, 1] },
  ...over,
});

const group = (id: string, name: string) => ({ id, name }) as NodeGroup;

describe('alertGrouping', () => {
  it('reads each level from its place in the preview', () => {
    expect(notifiedAt(cfg(), 'off')).toBe(7);
    expect(notifiedAt(cfg(), 'certain')).toBe(3);
    expect(notifiedAt(cfg(), 'site')).toBe(1);
  });

  it('reads a short preview as grouping nothing', () => {
    expect(notifiedAt(cfg({ now: { down: 4, notified: [] } }), 'links')).toBe(4);
  });

  it('names the folders and sorts them by name, keeping an unknown one by its id', () => {
    const rows = folderLevelRows(
      cfg({
        folders: [
          { group_id: 'b', level: 'site' },
          { group_id: 'a', level: 'certain' },
          { group_id: 'gone', level: 'off' },
        ],
      }),
      [group('a', 'Tokyo'), group('b', 'Osaka')],
    );
    expect(rows.map((r) => r.name)).toEqual(['gone', 'Osaka', 'Tokyo']);
    expect(rows[1]).toEqual({ groupId: 'b', name: 'Osaka', level: 'site' });
  });

  it('knows its four levels', () => {
    expect(['off', 'certain', 'links', 'site'].every(isGroupingLevel)).toBe(true);
    expect(isGroupingLevel('derived')).toBe(false);
  });
});
