// SPDX-License-Identifier: AGPL-3.0-only
// What the Interfaces list's MODE and VLAN cells say about a port (ADR-201).
//
// Kept out of `InterfacesTab.tsx` so the decisions run under Vitest (a `.tsx` never does): which
// word a port's mode is, how a VLAN list is written, and which VLANs a port carries — the last one
// is what the VLAN filter answers, and a member port answers through its aggregate.

import type { InterfaceRow, InterfaceVlan, VlanSpan } from '../../types/api';

/** The MODE column's values, in the order the filter offers them. `not_reported` is the WebUI's own:
 *  the row has no VLAN facts at all (no walk yet, or a make whose tables are not read), or the device
 *  answered in a way core could not place (`unknown`). */
export const VLAN_MODE_KEYS = [
  'access',
  'trunk',
  'hybrid',
  'member',
  'not_l2',
  'not_reported',
] as const;
export type VlanModeKey = (typeof VLAN_MODE_KEYS)[number];

/** The lowest and highest usable VLAN IDs (0 and 4095 are reserved). */
export const VLAN_MIN = 1;
export const VLAN_MAX = 4094;

/** A row's VLAN facts, or `null` — tolerating a core older than ADR-201, which sends no field. */
export function vlanOf(row: Pick<InterfaceRow, 'vlan'>): InterfaceVlan | null {
  return row.vlan ?? null;
}

/** The word a row's MODE cell (and the mode filter) uses. */
export function vlanModeKey(row: Pick<InterfaceRow, 'vlan'>): VlanModeKey {
  const v = vlanOf(row);
  if (v == null) return 'not_reported';
  switch (v.mode) {
    case 'access':
    case 'trunk':
    case 'hybrid':
    case 'member':
    case 'not_l2':
      return v.mode;
    case 'unknown':
      return 'not_reported';
  }
}

/** Whether `spans` is every usable VLAN. */
export function isAllVlans(spans: readonly VlanSpan[]): boolean {
  return spans.length === 1 && spans[0].first === VLAN_MIN && spans[0].last === VLAN_MAX;
}

/** Spans written the way a switch CLI writes them: `700,801-869,872-889`. */
export function formatSpans(spans: readonly VlanSpan[]): string {
  return spans.map((s) => (s.first === s.last ? `${s.first}` : `${s.first}-${s.last}`)).join(',');
}

/** How many VLANs `spans` covers. */
export function spanCount(spans: readonly VlanSpan[]): number {
  return spans.reduce((n, s) => n + (s.last - s.first + 1), 0);
}

function inSpans(spans: readonly VlanSpan[], v: number): boolean {
  return spans.some((s) => v >= s.first && v <= s.last);
}

/** Whether VLAN `v` crosses this port by its own configuration. A member says no here; its
 *  aggregate answers for it, through [`effectiveVlan`]. */
function carriesOwn(vlan: InterfaceVlan, v: number): boolean {
  switch (vlan.mode) {
    case 'access':
      return vlan.access_vlan === v || vlan.voice_vlan === v;
    case 'trunk':
      return vlan.native === v || inSpans(vlan.allowed, v);
    case 'hybrid':
      return inSpans(vlan.untagged, v) || inSpans(vlan.tagged, v);
    case 'member':
    case 'not_l2':
    case 'unknown':
      return false;
  }
}

/** A VLAN ID as the filter accepts it, or `null`. */
export function parseVlanId(token: string): string | null {
  const t = token.trim();
  if (!/^\d{1,4}$/.test(t)) return null;
  const n = Number(t);
  return n >= VLAN_MIN && n <= VLAN_MAX ? String(n) : null;
}

/** The parts the VLAN cell draws. `kind` says which shape; the component looks the words up. */
export type VlanCell =
  | { kind: 'access'; vlan: number | null; voice: number | null }
  | { kind: 'trunk'; native: number | null; allowed: string; all: boolean; count: number }
  | { kind: 'hybrid'; untagged: string; tagged: string }
  | { kind: 'member'; lag: string; lagIfindex: number }
  | { kind: 'not_l2' }
  | { kind: 'not_reported' };

/** What one row's VLAN cell says. */
export function vlanCell(row: Pick<InterfaceRow, 'vlan'>): VlanCell {
  const v = vlanOf(row);
  if (v == null) return { kind: 'not_reported' };
  switch (v.mode) {
    case 'access':
      return { kind: 'access', vlan: v.access_vlan ?? null, voice: v.voice_vlan ?? null };
    case 'trunk':
      return {
        kind: 'trunk',
        native: v.native ?? null,
        allowed: formatSpans(v.allowed),
        all: isAllVlans(v.allowed),
        count: spanCount(v.allowed),
      };
    case 'hybrid':
      return { kind: 'hybrid', untagged: formatSpans(v.untagged), tagged: formatSpans(v.tagged) };
    case 'member':
      return v.lag
        ? { kind: 'member', lag: v.lag.name ?? `if${v.lag.ifindex}`, lagIfindex: v.lag.ifindex }
        : { kind: 'not_l2' };
    case 'not_l2':
      return { kind: 'not_l2' };
    case 'unknown':
      return { kind: 'not_reported' };
  }
}

/** Whether a cell says more than its MODE word — `not_l2` and `not_reported` are the word itself,
 *  so a surface that draws the mode beside the cell would only repeat it. */
export function vlanCellHasDetail(cell: VlanCell): boolean {
  return cell.kind !== 'not_l2' && cell.kind !== 'not_reported';
}

/** An aggregate's member ports by name, or `[]` for any other port. */
export function memberNames(row: Pick<InterfaceRow, 'vlan'>): { name: string; ifindex: number }[] {
  return (vlanOf(row)?.members ?? []).map((m) => ({
    name: m.name ?? `if${m.ifindex}`,
    ifindex: m.ifindex,
  }));
}

/** The VLAN facts that decide which VLANs a row carries: its own, or — for a member — its
 *  aggregate's. The filter reads this, so the Interfaces tab attaches it to each row once. */
export function effectiveVlan(
  row: Pick<InterfaceRow, 'vlan'>,
  byIfindex: ReadonlyMap<number, Pick<InterfaceRow, 'vlan'>>,
): InterfaceVlan | null {
  const v = vlanOf(row);
  if (v?.mode === 'member' && v.lag) return vlanOf(byIfindex.get(v.lag.ifindex) ?? {}) ?? null;
  return v;
}

const carriedCache = new WeakMap<InterfaceVlan, readonly string[]>();

/** Every VLAN ID `vlan` carries, as filter tokens. Expanded once per VLAN object and cached — a
 *  trunk allowing everything is 4,094 tokens, and the filter re-reads every row on each keystroke. */
export function carriedVlanTokens(vlan: InterfaceVlan | null | undefined): readonly string[] {
  if (vlan == null) return [];
  const hit = carriedCache.get(vlan);
  if (hit) return hit;
  const out: string[] = [];
  for (let v = VLAN_MIN; v <= VLAN_MAX; v++) if (carriesOwn(vlan, v)) out.push(String(v));
  carriedCache.set(vlan, out);
  return out;
}

/** The words a VLAN cell is written with, looked up by the caller (`interfaces.vlan.*`). */
export interface VlanWords {
  native: string;
  noNative: string;
  all: string;
  untagged: string;
  tagged: string;
  voice: string;
  inLag: string;
  notL2: string;
  notReported: string;
}

/** One cell as plain text — the cell's `title` and the dock's tile, where nothing is cut off. */
export function vlanText(cell: VlanCell, w: VlanWords): string {
  switch (cell.kind) {
    case 'access':
      return [
        cell.vlan == null ? w.notReported : String(cell.vlan),
        cell.voice == null ? null : `+ ${w.voice} ${cell.voice}`,
      ]
        .filter(Boolean)
        .join(' ');
    case 'trunk':
      return `${cell.native == null ? w.noNative : `${w.native} ${cell.native}`} · ${
        cell.all ? w.all : cell.allowed || '—'
      }`;
    case 'hybrid':
      return `${w.untagged} ${cell.untagged || '—'} · ${w.tagged} ${cell.tagged || '—'}`;
    case 'member':
      return `${w.inLag} ${cell.lag}`;
    case 'not_l2':
      return w.notL2;
    case 'not_reported':
      return w.notReported;
  }
}
