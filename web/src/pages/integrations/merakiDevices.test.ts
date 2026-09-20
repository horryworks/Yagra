// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import { specColumns } from '../../lib/columnFilter';
import { buildPredicate } from '../../lib/filterPredicate';
import {
  MERAKI_DEVICE_STATES,
  MERAKI_FILING_REASONS,
  type MerakiDevice,
  type MerakiDeviceState,
} from '../../types/api';
import {
  MAX_DEVICES_MAX,
  MAX_DEVICES_MIN,
  MAX_DEVICES_RANGE,
  deviceDestination,
  deviceSearchText,
  devicesToImport,
  importableSerials,
  isImportable,
  merakiDeviceFilters,
  networkLabel,
  parseMaxDevices,
  pruneSelection,
  toImportDevice,
  uncollectedDevices,
  unwatchedNetworkIds,
  unwatchedNotice,
} from './merakiDevices';

const device = (over: Partial<MerakiDevice> = {}): MerakiDevice => ({
  serial: 'Q2XX-0001',
  name: 'edge-tokyo',
  model: 'MX67',
  product_type: 'appliance',
  network_id: 'N_1',
  network_name: 'Tokyo',
  network_monitored: true,
  lan_ip: '10.1.0.1',
  first_seen_at: '2026-09-01T00:00:00Z',
  missing_since: null,
  node_id: null,
  folder_id: null,
  filing: null,
  state: 'new',
  ...over,
});

describe('isImportable', () => {
  it('offers exactly the rows Meraki lists that are not nodes here', () => {
    const importable = MERAKI_DEVICE_STATES.filter((state) => isImportable({ state }));
    // `never_online` and `deleted` are the two the automatic import skips on purpose; if the manual
    // one skipped them too there would be no way to bring either in.
    expect(importable).toEqual(['new', 'never_online', 'deleted']);
  });

  it('never offers a row that is already a node', () => {
    // `missing` is the one that could be argued the other way: the node exists, Meraki just
    // stopped listing the device. Importing it again would be a second node for one serial.
    expect(isImportable({ state: 'monitored' })).toBe(false);
    expect(isImportable({ state: 'missing' })).toBe(false);
  });
});

describe('deviceDestination', () => {
  it('names the folder a node is in, with no reason — nothing would be filed', () => {
    expect(
      deviceDestination(device({ state: 'monitored', node_id: 'n-1', folder_id: 'g-1' })),
    ).toEqual({ destination: { kind: 'folder', folderId: 'g-1' }, note: null });
  });

  it('puts a node with no folder at the top of the tree, not under the organization', () => {
    // The null that means two things. Reading it as "by network" would print a folder path over a
    // node that is in no folder at all.
    for (const state of ['monitored', 'missing'] as MerakiDeviceState[]) {
      expect(deviceDestination(device({ state, node_id: 'n-1', folder_id: null }))).toEqual({
        destination: { kind: 'root' },
        note: null,
      });
    }
  });

  it('names the folder whose range claims the address, and says which range', () => {
    expect(
      deviceDestination(
        device({
          folder_id: 'g-tokyo',
          filing: { reason: 'matched', prefix: '10.1.0.0/16', folders: null },
        }),
      ),
    ).toEqual({
      destination: { kind: 'folder', folderId: 'g-tokyo' },
      note: { reason: 'matched', args: { prefix: '10.1.0.0/16' } },
    });
  });

  it('files everything else under the organization, by network, and says why', () => {
    const cases: [MerakiDevice['filing'], unknown][] = [
      [{ reason: 'unmatched' }, { reason: 'unmatched', args: {} }],
      [{ reason: 'no_address' }, { reason: 'no_address', args: {} }],
      [{ reason: 'not_asked' }, { reason: 'not_asked', args: {} }],
      [{ reason: 'ambiguous', folders: 3 }, { reason: 'ambiguous', args: { count: 3 } }],
    ];
    for (const [filing, note] of cases) {
      expect(deviceDestination(device({ filing }))).toEqual({
        destination: { kind: 'network', networkName: 'Tokyo' },
        note,
      });
    }
  });

  it('never says zero folders matched equally', () => {
    // "0 folders match equally" is a different finding from the one the server reported.
    const view = deviceDestination(device({ filing: { reason: 'ambiguous', folders: null } }));
    expect(view.note).toEqual({ reason: 'ambiguous', args: { count: 2 } });
  });

  it('has an answer for every reason the server can send', () => {
    // The `switch` is exhaustive at compile time; this is the runtime half, so a reason added to
    // the array and handled nowhere shows up as a missing note rather than as `undefined` text.
    for (const reason of MERAKI_FILING_REASONS) {
      expect(deviceDestination(device({ filing: { reason } })).note?.reason).toBe(reason);
    }
  });
});

describe('networkLabel', () => {
  it('falls back to the network id when the sync recorded no name', () => {
    expect(networkLabel({ network_id: 'N_9', network_name: null })).toBe('N_9');
    expect(networkLabel({ network_id: 'N_9', network_name: '  ' })).toBe('N_9');
    expect(networkLabel({ network_id: 'N_9', network_name: 'Osaka' })).toBe('Osaka');
  });
});

describe('unwatched networks', () => {
  const networks = [
    { network_id: 'N_1', monitored: true },
    { network_id: 'N_2', monitored: false },
    { network_id: 'N_3', monitored: false },
  ];

  it('lists the networks the organization does not watch', () => {
    expect(unwatchedNetworkIds(networks)).toEqual(['N_2', 'N_3']);
    expect(unwatchedNetworkIds([])).toEqual([]);
  });

  it('warns only while automatic import is on', () => {
    // With it off nothing is imported from any network, so "…so their devices are not imported"
    // would blame the network scope for what the switch above it is doing.
    expect(unwatchedNotice({ import_devices: true }, networks)).toEqual(['N_2', 'N_3']);
    expect(unwatchedNotice({ import_devices: false }, networks)).toEqual([]);
  });
});

describe('monitored devices nothing is collected for (ADR-164 決定 15)', () => {
  const at = (
    state: 'monitored' | 'new' | 'never_online' | 'deleted' | 'missing',
    network_id: string,
    network_monitored: boolean,
  ) => ({ state, network_id, network_monitored });

  it('counts only a monitored device whose network is not watched', () => {
    const got = uncollectedDevices([
      at('monitored', 'N_1', true),
      at('monitored', 'N_2', false),
      // No node, so nothing that could go quiet — wherever it sits.
      at('new', 'N_2', false),
      at('never_online', 'N_3', false),
      at('deleted', 'N_3', false),
      // Already reported as not found in Meraki; its network is moot.
      at('missing', 'N_4', false),
    ]);
    expect(got).toEqual({ count: 1, networkIds: ['N_2'] });
  });

  it('names only the networks those devices are in, each once', () => {
    // N_9 is unwatched too, and holds no node: watching it would start importing from it, which
    // is not what "bring these devices back" asked for.
    const got = uncollectedDevices([
      at('monitored', 'N_3', false),
      at('monitored', 'N_2', false),
      at('monitored', 'N_3', false),
      at('new', 'N_9', false),
    ]);
    expect(got).toEqual({ count: 3, networkIds: ['N_2', 'N_3'] });
  });

  it('is silent when every node is in a watched network', () => {
    expect(uncollectedDevices([at('monitored', 'N_1', true)])).toEqual({ count: 0, networkIds: [] });
    expect(uncollectedDevices([])).toEqual({ count: 0, networkIds: [] });
  });
});

describe('the import request', () => {
  it('carries the seven fields the endpoint reads, and nothing else', () => {
    const sent = toImportDevice(
      device({ node_id: null, folder_id: 'g-1', filing: { reason: 'matched', prefix: '10/8' } }),
    );
    expect(sent).toEqual({
      serial: 'Q2XX-0001',
      name: 'edge-tokyo',
      model: 'MX67',
      product_type: 'appliance',
      network_id: 'N_1',
      network_name: 'Tokyo',
      lan_ip: '10.1.0.1',
    });
    // In particular no `file_by_prefix` and no `folder_id`: where a device goes is decided by the
    // server from the organization's own setting, which is the point of this page not sending one.
    expect(Object.keys(sent).sort()).toEqual(
      ['lan_ip', 'model', 'name', 'network_id', 'network_name', 'product_type', 'serial'].sort(),
    );
  });

  it('sends null, not undefined, for what Meraki did not report', () => {
    const sent = toImportDevice(
      device({ model: undefined, network_name: undefined, lan_ip: null }),
    );
    expect(sent.model).toBeNull();
    expect(sent.network_name).toBeNull();
    expect(sent.lan_ip).toBeNull();
  });

  it('sends only rows that are ticked and still importable', () => {
    const devices = [
      device({ serial: 'A', state: 'new' }),
      device({ serial: 'B', state: 'never_online' }),
      // Ticked while it was `new`; automatic import got there before the button was pressed.
      device({ serial: 'C', state: 'monitored', node_id: 'n-c' }),
      device({ serial: 'D', state: 'deleted' }),
    ];
    const sent = devicesToImport(devices, new Set(['A', 'C', 'D', 'gone']));
    expect(sent.map((d) => d.serial)).toEqual(['A', 'D']);
  });
});

describe('importableSerials', () => {
  it('ticks only the rows that have a checkbox', () => {
    // "Select all" over a list that is mostly nodes must not put a node's serial in the selection:
    // the count on the Import button is the selection's size.
    const rows = MERAKI_DEVICE_STATES.map((state) => device({ serial: state, state }));
    expect(importableSerials(rows)).toEqual(['new', 'never_online', 'deleted']);
  });
});

describe('pruneSelection', () => {
  const devices = [
    device({ serial: 'A', state: 'new' }),
    device({ serial: 'B', state: 'monitored', node_id: 'n-b' }),
  ];

  it('drops a serial that became a node, or left the list', () => {
    expect([...pruneSelection(new Set(['A', 'B', 'gone']), devices)]).toEqual(['A']);
  });

  it('hands back the same set when nothing dropped', () => {
    // Identity, not equality: the page passes the result to `setState`, and a fresh Set every
    // reload would re-render the whole table for nothing.
    const selected = new Set(['A']);
    expect(pruneSelection(selected, devices)).toBe(selected);
  });
});

describe('parseMaxDevices', () => {
  it('accepts the whole range the server does, bounds included', () => {
    expect(parseMaxDevices(String(MAX_DEVICES_MIN))).toBe(1);
    expect(parseMaxDevices(String(MAX_DEVICES_MAX))).toBe(50_000);
    expect(parseMaxDevices(' 250 ')).toBe(250);
  });

  it('refuses what is outside it rather than clamping', () => {
    // 60000 silently saved as 50000 is a setting nobody chose.
    expect(parseMaxDevices('0')).toBeNull();
    expect(parseMaxDevices('50001')).toBeNull();
    expect(parseMaxDevices('-5')).toBeNull();
  });

  it('refuses anything that is not plain digits', () => {
    // `parseInt('12abc')` is 12 and `Number('1e3')` is 1000 — both are numbers nobody typed.
    for (const raw of ['', '  ', '12abc', '1e3', '1.5', '0x10', 'NaN']) {
      expect(parseMaxDevices(raw), raw).toBeNull();
    }
  });

  it('prints the same bounds it enforces', () => {
    expect(MAX_DEVICES_RANGE).toBe('1–50000');
  });
});

describe('merakiDeviceFilters', () => {
  const t = ((k: string) => k) as unknown as TFunction;
  const columns = specColumns(merakiDeviceFilters(t));
  const spec = (key: string) => columns.find((c) => c.key === key)!.filter;

  it('offers every state the server can send, in its order', () => {
    const state = spec('state');
    expect(state.kind).toBe('enum');
    if (state.kind !== 'enum') return;
    expect(state.options.map((o) => o.value)).toEqual([...MERAKI_DEVICE_STATES]);
    expect(state.readValue?.(device({ state: 'deleted' }))).toBe('deleted');
  });

  it('finds a device by its serial as well as its name', () => {
    expect(deviceSearchText(device())).toEqual(['edge-tokyo', 'Q2XX-0001']);
  });

  it('narrows to the states that were picked', () => {
    const rows = [
      device({ serial: 'A', state: 'new' }),
      device({ serial: 'B', state: 'monitored' }),
      device({ serial: 'C', state: 'never_online' }),
    ];
    const keep = buildPredicate(columns, { name: '', network: '', state: 'new,never_online' }, 0);
    expect(rows.filter(keep).map((d) => d.serial)).toEqual(['A', 'C']);
  });

  it('matches the network by what the cell shows, including the id fallback', () => {
    const rows = [
      device({ serial: 'A', network_name: 'Tokyo' }),
      device({ serial: 'B', network_id: 'N_osaka', network_name: null }),
    ];
    const keep = buildPredicate(columns, { name: '', network: 'osaka', state: '' }, 0);
    expect(rows.filter(keep).map((d) => d.serial)).toEqual(['B']);
  });
});
