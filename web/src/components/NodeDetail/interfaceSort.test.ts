// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { sortRows } from '../../lib/tableSort';
import type { Neighbor } from '../../types/api';
import { INTERFACE_COLUMNS } from './interfaceColumns';
import {
  DEFAULT_INTERFACE_SORT,
  INTERFACE_SORT_KEYS,
  interfaceSortValues,
  type SortableInterface,
} from './interfaceSort';

const port = (p: Partial<SortableInterface> & { ifindex: number }) => p as SortableInterface;

const rows = [
  port({ ifindex: 10, if_name: 'Gi1/0/10', oper_status: 2, if_speed_bps: 0 }),
  port({
    ifindex: 2,
    if_name: 'Gi1/0/2',
    oper_status: 1,
    if_speed_bps: 1e9,
    in_bps: 5e8,
  }),
  port({
    ifindex: 9,
    if_name: 'Gi1/0/9',
    oper_status: null,
    if_speed_bps: 1e8,
  }),
  port({
    ifindex: 1,
    if_name: 'Te1/1/1',
    oper_status: 1,
    if_speed_bps: 1e10,
    in_bps: 2e9,
  }),
];
const values = interfaceSortValues(() => undefined);
const names = (list: SortableInterface[]) => list.map((r) => r.if_name);

describe('interface sort', () => {
  it('names a value for every column but the sparkline, in the header order', () => {
    const header = INTERFACE_COLUMNS.map((c) => c.key).filter((k) => k !== 'throughput');
    expect([...INTERFACE_SORT_KEYS]).toEqual(header);
    expect(Object.keys(values).sort()).toEqual([...INTERFACE_SORT_KEYS].sort());
    expect(INTERFACE_SORT_KEYS).toContain(DEFAULT_INTERFACE_SORT.by);
  });

  it('orders the Interface column the way the device numbers its ports', () => {
    expect(names(sortRows(rows, { by: 'if_name', dir: 'asc' }, values))).toEqual([
      'Te1/1/1',
      'Gi1/0/2',
      'Gi1/0/9',
      'Gi1/0/10',
    ]);
  });

  it('sorts speed on bps and puts a port with no speed last both ways', () => {
    expect(names(sortRows(rows, { by: 'speed', dir: 'desc' }, values))).toEqual([
      'Te1/1/1',
      'Gi1/0/2',
      'Gi1/0/9',
      'Gi1/0/10',
    ]);
    expect(names(sortRows(rows, { by: 'speed', dir: 'asc' }, values)).at(-1)).toBe('Gi1/0/10');
  });

  it('puts up before down, and a port whose state is unknown last', () => {
    expect(names(sortRows(rows, { by: 'oper', dir: 'asc' }, values))).toEqual([
      'Gi1/0/2',
      'Te1/1/1',
      'Gi1/0/10',
      'Gi1/0/9',
    ]);
  });

  it('sorts traffic only on ports that are up, so a down port never reads as quiet', () => {
    expect(names(sortRows(rows, { by: 'in', dir: 'desc' }, values)).slice(0, 2)).toEqual([
      'Te1/1/1',
      'Gi1/0/2',
    ]);
    expect(values.in(rows[0])).toBeNull();
  });

  it('sorts neighbours by the peer the cell shows', () => {
    const peer = { remote_sys_name: 'sw-02', remote_chassis: 'aa' } as Neighbor;
    const v = interfaceSortValues((ifindex) => (ifindex === 2 ? [peer] : undefined));
    expect(v.neighbors(rows[1])).toBe('sw-02');
    expect(v.neighbors(rows[0])).toBeNull();
  });
});
