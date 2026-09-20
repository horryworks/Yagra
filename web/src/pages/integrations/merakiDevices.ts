// SPDX-License-Identifier: AGPL-3.0-only
// What one Meraki organization's page decides about its devices (ADR-164 Inc.4/5): which rows can
// be imported, where each one is or would be filed, which networks keep automatic import from
// reaching their devices, what an import request carries, and whether a typed cap is usable.
//
// A `.ts` because Vitest never loads a `.tsx` (`testing.md`), and `tsxJudgement.test.ts` fails the
// build for judgement left in one. `MerakiOrgPage.tsx` is layout over these.
//
// **Why the list is filtered in the browser.** `GET /meraki/orgs/{id}/devices` answers the whole
// inventory in one response — there is no cursor — so the page *is* the list, the same position
// the node detail's AP tab is in. Filtering on the server would mean a second endpoint; filtering
// here costs a pass over rows that are already in hand.
//
// ⚠️ This module builds a filter row, so it has a line in `lib/filterSpecRegistry.test.ts`.

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../../lib/columnFilter';
import {
  MERAKI_DEVICE_STATES,
  type MerakiDevice,
  type MerakiDeviceState,
  type MerakiFilingReason,
  type MerakiNetwork,
  type MerakiOrg,
} from '../../types/api';

// ───────────────────────────────────────────────────────────────────── states

/** What each state means for the two questions the page asks of a row.
 *
 *  A `Record` over the union, so a sixth state is a compile error here rather than a row that is
 *  silently neither importable nor a node (`extensibility.md` §1). */
const STATE_SPECS: Record<MerakiDeviceState, { isNode: boolean; importable: boolean }> = {
  // A node already. `missing` is one too: the node stays, Meraki just stopped listing the device.
  monitored: { isNode: true, importable: false },
  missing: { isNode: true, importable: false },
  // Listed, and not a node. `never_online` and `deleted` are exactly the two the automatic import
  // leaves alone on purpose — which is why the manual one has to be able to take them.
  new: { isNode: false, importable: true },
  never_online: { isNode: false, importable: true },
  deleted: { isNode: false, importable: true },
};

/** Whether a row gets a checkbox: it is listed by Meraki and is not a node here. */
export function isImportable(device: Pick<MerakiDevice, 'state'>): boolean {
  return STATE_SPECS[device.state].importable;
}

// ──────────────────────────────────────────────────────────────── destination

/** Where a device is, or where an import would put it. */
export type DeviceDestination =
  /** A folder that exists: the node's own, or the one whose IP range claims the address. */
  | { kind: 'folder'; folderId: string }
  /** Under the organization's own folder, in one named after the device's network. That folder is
   *  created by the import that first needs it, so there is no id to resolve — only the name. */
  | { kind: 'network'; networkName: string }
  /** A node that sits at the top of the tree, in no folder at all. */
  | { kind: 'root' };

/** Why an import would file the device there. Rendered as
 *  `` t(`meraki.devices.filing.${reason}`, args) `` — the key is built at the call site so
 *  `i18nPrefixes.test.ts` can check its spelling. */
export interface FilingNote {
  reason: MerakiFilingReason;
  args: Record<string, unknown>;
}

export interface DeviceDestinationView {
  destination: DeviceDestination;
  /** `null` for a device that is already a node: nothing would be filed, so there is no why. */
  note: FilingNote | null;
}

type DestinationFields = Pick<
  MerakiDevice,
  'state' | 'folder_id' | 'filing' | 'network_id' | 'network_name'
>;

/** What the Destination cell says about one row.
 *
 *  🚨 **A null `folder_id` means two different things**, and the state is what tells them apart:
 *  on a node it is "at the top of the tree", on a device that is not one it is "under the
 *  organization's folder, by network". Reading both as the second would print a folder path over a
 *  node that is in no folder. */
export function deviceDestination(device: DestinationFields): DeviceDestinationView {
  const note = filingNote(device.filing);
  if (device.folder_id) {
    return { destination: { kind: 'folder', folderId: device.folder_id }, note };
  }
  if (STATE_SPECS[device.state].isNode) return { destination: { kind: 'root' }, note };
  return {
    destination: { kind: 'network', networkName: networkLabel(device) },
    note,
  };
}

/** A network's name, or its id for one the sync has not recorded a name for. */
export function networkLabel(device: Pick<MerakiDevice, 'network_id' | 'network_name'>): string {
  return device.network_name?.trim() || device.network_id;
}

function filingNote(filing: MerakiDevice['filing']): FilingNote | null {
  if (!filing) return null;
  switch (filing.reason) {
    case 'matched':
      return { reason: 'matched', args: { prefix: filing.prefix ?? '' } };
    case 'ambiguous':
      // The server sends the count with this reason. Two is what the word itself guarantees, so it
      // is the floor for a body that arrived without one — never zero, which would read as a
      // different finding.
      return { reason: 'ambiguous', args: { count: filing.folders ?? 2 } };
    case 'unmatched':
    case 'no_address':
    case 'not_asked':
      return { reason: filing.reason, args: {} };
  }
}

// ─────────────────────────────────────────────────────────────────── networks

/** The networks automatic import cannot reach, because the organization does not watch them. */
export function unwatchedNetworkIds(
  networks: readonly Pick<MerakiNetwork, 'network_id' | 'monitored'>[],
): string[] {
  return networks.filter((n) => !n.monitored).map((n) => n.network_id);
}

/** Whether the "N networks are not watched" notice is shown, and for how many.
 *
 *  Only while automatic import is on: with it off nothing is imported from *any* network, so the
 *  sentence "…so their devices are not imported" would blame the wrong switch. */
export function unwatchedNotice(
  org: Pick<MerakiOrg, 'import_devices'>,
  networks: readonly Pick<MerakiNetwork, 'network_id' | 'monitored'>[],
): string[] {
  return org.import_devices ? unwatchedNetworkIds(networks) : [];
}

/** The monitored devices nothing is collected for, and the networks that would have to be watched
 *  to fix that (ADR-164 決定 15).
 *
 *  Collection asks the Dashboard about watched networks only, so a node whose network is not
 *  watched receives nothing: it keeps the last state it was seen in and raises no alert. That
 *  happens when a device is moved into an unwatched network, and when a network that holds nodes
 *  is un-watched.
 *
 *  ⚠️ `networkIds` is **only the networks these devices are in** — deliberately not
 *  [`unwatchedNetworkIds`]. That list answers a different question (what automatic import cannot
 *  reach), and watching every network to bring two nodes back would start importing from all of
 *  them. Only `monitored` counts: a device with no node has nothing that could go quiet, and one
 *  Meraki no longer lists (`missing`) is already reported as such. */
export function uncollectedDevices(
  devices: readonly Pick<MerakiDevice, 'state' | 'network_id' | 'network_monitored'>[],
): { count: number; networkIds: string[] } {
  const quiet = devices.filter((d) => d.state === 'monitored' && !d.network_monitored);
  return {
    count: quiet.length,
    networkIds: [...new Set(quiet.map((d) => d.network_id))].sort(),
  };
}

// ───────────────────────────────────────────────────────────────────── import

/** One device as `POST /meraki/import` wants it. */
export interface MerakiImportDevice {
  serial: string;
  name: string;
  model: string | null;
  product_type: string;
  network_id: string;
  network_name: string | null;
  lan_ip: string | null;
}

/** A row, as the import request carries it. */
export function toImportDevice(device: MerakiDevice): MerakiImportDevice {
  return {
    serial: device.serial,
    name: device.name,
    model: device.model ?? null,
    product_type: device.product_type,
    network_id: device.network_id,
    network_name: device.network_name ?? null,
    lan_ip: device.lan_ip ?? null,
  };
}

/** The devices an "Import N devices" press sends: ticked **and still importable**.
 *
 *  The second half is not redundant. The list is reloaded by a sync, and a row that was `new` when
 *  it was ticked can be `monitored` a moment later — automatic import got there first. */
export function devicesToImport(
  devices: readonly MerakiDevice[],
  selected: ReadonlySet<string>,
): MerakiImportDevice[] {
  return devices.filter((d) => selected.has(d.serial) && isImportable(d)).map(toImportDevice);
}

/** Every serial in `devices` that can be imported — what "Select all importable" ticks.
 *
 *  The page hands it the rows the filter row is *showing*, not the whole list, so "State: New" +
 *  "select all" is how an operator says "import everything new" without meaning the 400 devices
 *  they deleted on purpose. */
export function importableSerials(
  devices: readonly Pick<MerakiDevice, 'serial' | 'state'>[],
): string[] {
  return devices.filter(isImportable).map((d) => d.serial);
}

/** The selection, minus every serial that can no longer be imported — what is kept across a
 *  reload. Returns the same set when nothing dropped, so a caller can hand it to `setState`
 *  without causing a render. */
export function pruneSelection(
  selected: ReadonlySet<string>,
  devices: readonly MerakiDevice[],
): ReadonlySet<string> {
  const importable = new Set(devices.filter(isImportable).map((d) => d.serial));
  const kept = [...selected].filter((serial) => importable.has(serial));
  return kept.length === selected.size ? selected : new Set(kept);
}

// ──────────────────────────────────────────────────────────────────── the cap

/** The bounds `PUT …/import-settings` accepts for `max_devices`. Outside them the server answers
 *  400 `invalid_max_devices`; the form refuses first so the operator is told before pressing. */
export const MAX_DEVICES_MIN = 1;
export const MAX_DEVICES_MAX = 50_000;

/** What the cap's hint line reads. Built from the bounds so the two cannot disagree. */
export const MAX_DEVICES_RANGE = `${MAX_DEVICES_MIN}–${MAX_DEVICES_MAX}`;

/** The cap a text box holds, or `null` when it is not one the server would take.
 *
 *  Digits only, on purpose: `parseInt('12abc')` is 12 and `Number('1e3')` is 1000, and saving a
 *  number the operator did not type is worse than asking them to retype it. Not clamped for the
 *  same reason — 60000 silently becoming 50000 is a setting nobody chose. */
export function parseMaxDevices(raw: string): number | null {
  const text = raw.trim();
  if (!/^\d+$/.test(text)) return null;
  const n = Number(text);
  return n >= MAX_DEVICES_MIN && n <= MAX_DEVICES_MAX ? n : null;
}

// ───────────────────────────────────────────────────────────────── filter row

/** What the Name filter reads: the name the row shows, and the serial under it. */
export function deviceSearchText(device: Pick<MerakiDevice, 'name' | 'serial'>): string[] {
  return [device.name, device.serial];
}

/**
 * The device list's filter row, keyed by `Column.key` (ADR-053).
 *
 * State is the one that earns the row: "which of these is not monitored yet" is the question the
 * page is opened with. Name and Network are here because an organization's list runs to thousands
 * of rows, and a list that long with no way to find one device is a list nobody reads.
 */
export function merakiDeviceFilters(t: TFunction): Record<string, ColumnFilterSpec<MerakiDevice>> {
  return {
    name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: deviceSearchText,
      containsSemantics: 'substring',
      placeholder: t('meraki.devices.cols.name'),
    },
    network: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      // Through `networkLabel`, so typing what the cell shows finds the row even when the sync
      // recorded no name and the cell fell back to the id.
      readText: (d) => [networkLabel(d)],
      containsSemantics: 'substring',
      placeholder: t('meraki.devices.cols.network'),
    },
    state: {
      kind: 'enum',
      options: MERAKI_DEVICE_STATES.map((s) => ({
        value: s,
        label: t(`meraki.devices.state.${s}`),
      })),
      readValue: (d) => d.state,
      allLabel: t('meraki.devices.allStates'),
      counts: 'client',
    },
  };
}
