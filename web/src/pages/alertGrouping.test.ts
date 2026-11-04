// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { AlertGroupingConfig, NodeGroup } from '../types/api';
import { folderLevelRows, isGroupingLevel, notifiedAt, replayAt } from './alertGrouping';

const cfg = (over: Partial<AlertGroupingConfig> = {}): AlertGroupingConfig => ({
  level: 'off',
  wait: false,
  folders: [],
  now: { down: 7, notified: [7, 3, 3, 1] },
  last_7_days: null,
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
  it('reads the last seven days at a level, and leaves the line out when there is no replay', () => {
    expect(replayAt(cfg(), 'certain')).toBeNull();
    const last_7_days = {
      fired: 120,
      truncated: false,
      levels: [
        { level: 'off' as const, notified: 120, notified_waiting: 120 },
        { level: 'certain' as const, notified: 80, notified_waiting: 64, delay_max_secs: 300 },
      ],
    };
    expect(replayAt(cfg({ last_7_days }), 'certain')).toEqual({
      fired: 120,
      atOnce: 80,
      waiting: 64,
      maxDelaySecs: 300,
      truncated: false,
    });
    expect(replayAt(cfg({ last_7_days }), 'off')?.maxDelaySecs).toBeNull();
    // A level the server did not answer for groups nothing.
    expect(replayAt(cfg({ last_7_days }), 'site')).toMatchObject({ atOnce: 120, waiting: 120 });
  });
});
