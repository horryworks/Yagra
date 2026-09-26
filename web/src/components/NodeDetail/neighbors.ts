// SPDX-License-Identifier: AGPL-3.0-only
// Presentation logic for the Neighbors tab (ADR-038), kept out of the .tsx so it can be tested.
//
// Vitest runs with `environment: 'node'` and `include: ['src/**/*.test.ts']`, so a test written in
// a .tsx file is never executed. Anything here that makes a judgement — what counts as a change,
// how a row is labelled, whether the empty state means "nothing recorded" or "nothing connected" —
// lives on this side of that line (testing.md).

import { encodeCondition } from '../../lib/filterCondition';
import {
  NEIGHBOR_PEER_STATES,
  type CurrentNeighbors,
  type Neighbor,
  type NeighborPeer,
  type NeighborSet,
} from '../../types/api';
import { DISCOVERY_TABS } from '../../pages/discoveredEndpoints';
import { ENDPOINT_FILTER_PREFIX } from '../../pages/discoveryFilters';

/** How one adjacency differs between two consecutive observations. */
export type NeighborDiffKind = 'added' | 'removed' | 'changed';

/** One line of a history entry's summary. */
export interface NeighborDiffRow {
  kind: NeighborDiffKind;
  /** The adjacency as of the newer observation (or the older one, for `removed`). */
  neighbor: Neighbor;
}

/** Joins the parts of a lookup key. A NUL cannot occur inside any of the parts, so two different
 *  tuples never join to the same string. Built from its code point rather than written literally:
 *  a raw NUL in the source made git treat this file as binary, so its diffs could not be read. */
const KEY_SEP = String.fromCharCode(0);

/** The identity three-tuple plus protocol — what makes two observations "the same link".
 *
 *  Must match `yagra_common::Neighbor::identity` exactly, or a re-rendered row would read as a
 *  remove+add pair. It is only a display key here (the backend decides what a real change is), but
 *  a mismatch would make the UI describe changes the history does not record. */
function identity(n: Neighbor): string {
  return [n.proto, n.local_port, n.remote_chassis, n.remote_port].join(KEY_SEP);
}

/** Everything except the identity — what makes a link "changed" rather than replaced.
 *
 *  Mirrors the backend's content key, which is why `remote_chassis_kind` / `remote_port_kind` are
 *  **not** here: they say how an id was rendered, not what it is, and the key leaves them out so the
 *  first walk after an upgrade is not a change (ADR-180 決定 4). */
function payload(n: Neighbor): string {
  return JSON.stringify([
    n.local_ifindex ?? null,
    n.remote_port_desc ?? null,
    n.remote_sys_name ?? null,
    n.remote_sys_desc ?? null,
    n.remote_mgmt_addr ?? null,
    n.remote_platform ?? null,
    [...(n.capabilities ?? [])].sort(),
  ]);
}

/** What changed between the previous observation and this one.
 *
 *  `previous` is `null` for the genesis row (the first observation ever recorded for a node), where
 *  every adjacency is genuinely new rather than "added" relative to something. Rows come back in
 *  the order the server sent them, which is already canonical.
 */
export function diffNeighbors(
  previous: NeighborSet | null,
  current: NeighborSet,
): NeighborDiffRow[] {
  const before = new Map((previous?.neighbors ?? []).map((n) => [identity(n), n]));
  const rows: NeighborDiffRow[] = [];
  for (const n of current.neighbors ?? []) {
    const was = before.get(identity(n));
    if (was == null) {
      rows.push({ kind: 'added', neighbor: n });
    } else if (payload(was) !== payload(n)) {
      rows.push({ kind: 'changed', neighbor: n });
    }
    before.delete(identity(n));
  }
  // Whatever is left was present before and is not present now.
  for (const n of before.values()) rows.push({ kind: 'removed', neighbor: n });
  return rows;
}

/** Why the Neighbors tab has nothing to show — the three cases mean different things and an
 *  operator needs to be able to tell them apart.
 *
 *  - `disabled`  — collection is switched off deployment-wide; nothing is being walked.
 *  - `unrecorded`— nothing has ever been recorded for this node (not an SNMP device, or not yet
 *                  walked since collection was enabled).
 *  - `none`      — the device *was* walked and genuinely reports no neighbours.
 */
export type NeighborEmptyReason = 'disabled' | 'unrecorded' | 'none';

export function emptyReason(
  collectionEnabled: boolean,
  current: { neighbors: NeighborSet } | null,
): NeighborEmptyReason | null {
  if (current != null && (current.neighbors.neighbors ?? []).length > 0) return null;
  // Order matters: a deployment that turned collection off after data was recorded still has that
  // data, and showing it with a "collection is off" note is more useful than hiding it.
  if (current == null) return collectionEnabled ? 'unrecorded' : 'disabled';
  return 'none';
}

/** A stable React key for a row. The identity is unique within a set (the backend dedups on it),
 *  so no index is needed — which keeps keys stable as the set changes. */
export function neighborKey(n: Neighbor): string {
  return identity(n);
}

/** The peer's most useful human label: its system name when it published one, else the chassis id
 *  (which for LLDP is often a bare MAC — accurate but not a name). */
export function peerLabel(n: Neighbor): string {
  const name = n.remote_sys_name?.trim();
  return name != null && name !== '' ? name : n.remote_chassis;
}

/** Whether the peer label above is already the chassis id, so a UI can avoid printing it twice. */
export function peerLabelIsChassis(n: Neighbor): boolean {
  return peerLabel(n) === n.remote_chassis;
}

// ─────────────────────────────────────────────────── the Interfaces list's Neighbors column

/** The fields of an interface row that decide which neighbours sit on it — structural, so a test
 *  needs no full row. */
export interface NeighborPort {
  ifindex: number;
  if_name?: string | null;
}

/** How a port name is compared: trimmed and case-folded, nothing more. */
function portKey(name: string | null | undefined): string | null {
  const k = name?.trim().toLowerCase();
  return k ? k : null;
}

/**
 * Which neighbours sit on which interface row, keyed by `ifindex` (ADR-145).
 *
 * 1. A neighbour carrying `local_ifindex` (CDP) goes to the row with that ifIndex and nowhere else.
 *    Its name is not consulted — CDP names the port `ifindex <n>` when the naming table has no row.
 * 2. One without (LLDP) goes to the row whose `if_name` equals its `local_port`, ignoring case and
 *    surrounding space. When two rows share that name it goes to neither: ambiguous is not a match.
 * 3. Never `if_alias` (an operator's description repeats across ports), and never a numeric
 *    `local_port` read as an ifIndex — the poller refuses that same guess for `lldpLocPortNum`.
 *
 * ⚠️ So an LLDP device that names a port differently from its `ifName` — `Gi0/3` against
 * `GigabitEthernet0/3`, or Junos's bare `994` against `et-0/2/0` — shows no neighbour on that row,
 * although the Neighbors tab lists one. On the lab's recorded sets 11 of 13 LLDP rows matched.
 */
export function neighborsByPort(
  neighbors: readonly Neighbor[],
  ports: readonly NeighborPort[],
): Map<number, Neighbor[]> {
  const ifindexes = new Set(ports.map((p) => p.ifindex));
  // `null` marks a name two rows share.
  const byName = new Map<string, number | null>();
  for (const p of ports) {
    const k = portKey(p.if_name);
    if (k == null) continue;
    byName.set(k, byName.has(k) ? null : p.ifindex);
  }
  const out = new Map<number, Neighbor[]>();
  for (const n of neighbors) {
    let target: number | null;
    if (n.local_ifindex != null) {
      target = ifindexes.has(n.local_ifindex) ? n.local_ifindex : null;
    } else {
      const k = portKey(n.local_port);
      target = k == null ? null : (byName.get(k) ?? null);
    }
    if (target == null) continue;
    const list = out.get(target);
    if (list) list.push(n);
    else out.set(target, [n]);
  }
  return out;
}

/** What the Neighbors cell on one interface row says (ADR-145). */
export interface NeighborCellText {
  /** The first neighbour's label — the one the cell shows. */
  label: string;
  /** How many more sit on the port; the cell adds `+N` when this is above zero. */
  more: number;
  /** Every neighbour on the port, one per line, for the cell's `title` — the cell ellipsizes. */
  title: string;
}

/** The cell's text, or `null` for a port with no neighbour (the cell then shows a dash). */
export function neighborCellText(list: readonly Neighbor[] | undefined): NeighborCellText | null {
  if (!list || list.length === 0) return null;
  return {
    label: peerLabel(list[0]),
    more: list.length - 1,
    title: list
      .map((n) => (n.remote_port ? `${peerLabel(n)} ${n.remote_port}` : peerLabel(n)))
      .join('\n'),
  };
}

// ─────────────────────────────────────────── what the tab adds to a row (ADR-180)

/** What the server said about each advertised address and each MAC-labelled id, keyed by the text
 *  the rows carry. Built once per response so every cell is a map lookup. */
export interface NeighborLookups {
  peers: ReadonlyMap<string, NeighborPeer>;
  vendors: ReadonlyMap<string, string>;
}

export const NO_LOOKUPS: NeighborLookups = { peers: new Map(), vendors: new Map() };

/** Index the two per-response lists. Absent lists (an older core) read as "nothing known", which
 *  shows every row exactly as it looked before this was added. */
export function neighborLookups(
  current: Pick<CurrentNeighbors, 'peers' | 'mac_vendors'> | null | undefined,
): NeighborLookups {
  if (!current) return NO_LOOKUPS;
  return {
    peers: new Map((current.peers ?? []).map((p) => [p.address, p])),
    vendors: new Map((current.mac_vendors ?? []).map((v) => [v.mac, v.vendor])),
  };
}

/** The server's verdict on this row's management address, or `null` when it advertised none. */
export function peerOf(n: Neighbor, lookups: NeighborLookups): NeighborPeer | null {
  const addr = n.remote_mgmt_addr;
  return addr ? (lookups.peers.get(addr) ?? null) : null;
}

/** What the Address column says about a row: the server's verdict on its address, or `none` when
 *  the neighbour advertised no address at all — a state worth filtering for on its own, since such a
 *  peer can never be matched to anything. */
export const NEIGHBOR_ADDRESS_STATES = [...NEIGHBOR_PEER_STATES, 'none'] as const;
export type NeighborAddressState = (typeof NEIGHBOR_ADDRESS_STATES)[number];

/** A row's address state. An address the server did not classify (an older core, or text that is
 *  not an address) reads as `null` — no state rather than a guessed one. */
export function neighborAddressState(
  n: Neighbor,
  lookups: NeighborLookups,
): NeighborAddressState | null {
  if (!n.remote_mgmt_addr) return 'none';
  return peerOf(n, lookups)?.state ?? null;
}

/** The registered maker of the chassis id — only when the device labelled it a MAC. The server
 *  already refuses anything else; asking the kind here as well means a row whose chassis happens to
 *  equal another row's MAC-labelled port id cannot borrow its vendor. */
export function chassisVendor(n: Neighbor, lookups: NeighborLookups): string | null {
  return n.remote_chassis_kind === 'mac' ? (lookups.vendors.get(n.remote_chassis) ?? null) : null;
}

/** The registered maker of the port id, on the same rule as [`chassisVendor`]. */
export function portVendor(n: Neighbor, lookups: NeighborLookups): string | null {
  return n.remote_port_kind === 'mac' ? (lookups.vendors.get(n.remote_port) ?? null) : null;
}

/** The Neighbor cell's second line: the chassis id when the first line is a name, plus its maker. */
export function peerSecondary(n: Neighbor, lookups: NeighborLookups): string | null {
  const parts: string[] = [];
  if (!peerLabelIsChassis(n)) parts.push(n.remote_chassis);
  const vendor = chassisVendor(n, lookups);
  if (vendor) parts.push(vendor);
  return parts.length > 0 ? parts.join(' · ') : null;
}

/** The Model / OS cell. CDP has a short platform string and, since ADR-180, its version banner in
 *  `remote_sys_desc`; LLDP has only the sysDescr. So the platform leads when there is one and the
 *  banner goes underneath; otherwise the description stands alone. */
export function platformCell(n: Neighbor): { primary: string | null; secondary: string | null } {
  const platform = n.remote_platform?.trim() || null;
  const desc = n.remote_sys_desc?.trim() || null;
  if (platform) return { primary: platform, secondary: desc };
  return { primary: desc, secondary: null };
}

/** One labelled line of the opened row. `labelKey` is under `nodes:neighbors.detail.`. */
export interface NeighborDetail {
  labelKey: NeighborDetailKey;
  value: string;
  mono: boolean;
}

export const NEIGHBOR_DETAIL_KEYS = [
  'sysName',
  'chassis',
  'chassisVendor',
  'port',
  'portVendor',
  'portDesc',
  'mgmtAddr',
  'platform',
  'sysDesc',
  'localIfindex',
] as const;
export type NeighborDetailKey = (typeof NEIGHBOR_DETAIL_KEYS)[number];

/** Every field the row carries, in reading order, leaving out the ones the device did not send.
 *  The opened row shows these in full and wrapped — the table cells above it ellipsize. */
export function neighborDetails(n: Neighbor, lookups: NeighborLookups): NeighborDetail[] {
  const rows: [NeighborDetailKey, string | null | undefined, boolean][] = [
    ['sysName', n.remote_sys_name, false],
    ['chassis', n.remote_chassis, true],
    ['chassisVendor', chassisVendor(n, lookups), false],
    ['port', n.remote_port, true],
    ['portVendor', portVendor(n, lookups), false],
    ['portDesc', n.remote_port_desc, false],
    ['mgmtAddr', n.remote_mgmt_addr, true],
    ['platform', n.remote_platform, false],
    ['sysDesc', n.remote_sys_desc, false],
    ['localIfindex', n.local_ifindex == null ? null : String(n.local_ifindex), true],
  ];
  return rows
    .filter(([, v]) => v != null && v.trim() !== '')
    .map(([labelKey, value, mono]) => ({ labelKey, value: value as string, mono }));
}

/** Where the peer's inventory entry is, when exactly one visible node owns its address. */
export function peerNodePath(peer: NeighborPeer | null): string | null {
  return peer?.state === 'node' && peer.node_id ? `/nodes/${peer.node_id}` : null;
}

/** Escape a string for use as a literal inside a regular expression. */
function regexLiteral(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * Discovery ▸ Unregistered, filtered to this address — or `null` when the list would not show it.
 *
 * The filter is an **anchored regex**, not a plain term: the address column matches substrings, so
 * `192.0.2.1` would also keep `192.0.2.10`. It goes through the list's own codec and URL prefix, so
 * the page reads it back as if the operator had typed it.
 */
export function discoveryPath(peer: NeighborPeer | null): string | null {
  if (!peer || peer.state !== 'unregistered' || !peer.discovery_listed) return null;
  const params = new URLSearchParams();
  const tab: (typeof DISCOVERY_TABS)[number] = 'unregistered';
  params.set('tab', tab);
  params.set(
    `${ENDPOINT_FILTER_PREFIX}ip`,
    encodeCondition({ term: `^${regexLiteral(peer.address)}$`, mode: 'regex', not: false }),
  );
  return `/nodes/discovery?${params.toString()}`;
}
