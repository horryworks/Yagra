// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Nodes ▸ Discovery filters (no DOM — Vitest node env).
//
// Rewritten for ADR-053 Inc.6, where both tables moved onto the shared column model. Every property
// asserted before still is; the additions are the per-column split and the set values the two
// checkboxes could not express.

import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import type { DiscoveredEndpoint, DiscoveryCandidate } from '../types/api';
import { defaultFilters, isAnyFiltered, type FilterState } from '../lib/columnFilter';
import { buildPredicate } from '../lib/filterPredicate';
import {
  candidateColumns,
  candidateLabels,
  endpointColumns,
  endpointLabels,
  ENDPOINT_DEFAULT_MONITORED,
} from './discoveryFilters';

const t = ((k: string) => k) as unknown as TFunction;

const cand = (over: Partial<DiscoveryCandidate> = {}): DiscoveryCandidate => ({
  address: '192.168.1.10',
  reachable: true,
  sysname: 'rtr-01',
  sysdescr: 'Cisco IOS Software',
  vendor: 'Cisco',
  model: 'ISR4331',
  sysobjectid: null,
  matched_credential_id: null,
  suggested_profile_id: null,
  ...over,
});

const ep = (over: Partial<DiscoveredEndpoint> = {}): DiscoveredEndpoint => ({
  id: 'e1',
  ip: '192.168.1.50',
  mac: 'aa:bb:cc:dd:ee:ff',
  via_node: 'sw-core',
  via_ifindex: 3,
  promoted_node_id: null,
  first_seen: '2026-08-01T00:00:00.000Z',
  last_seen: '2026-08-13T00:00:00.000Z',
  ...over,
});

const C_COLS = candidateColumns(t);
const C_DEFAULTS = defaultFilters(C_COLS);
const cf = (over: FilterState): FilterState => ({ ...C_DEFAULTS, ...over });
const hasCand = (row: DiscoveryCandidate, s: FilterState) => buildPredicate(C_COLS, s, 0)(row);

const E_COLS = endpointColumns(t);
// ⚠️ Not `defaultFilters`: this table's default narrows. The page builds the same object.
const E_DEFAULTS: FilterState = {
  ...defaultFilters(E_COLS),
  monitored: ENDPOINT_DEFAULT_MONITORED,
};
const ef = (over: FilterState): FilterState => ({ ...E_DEFAULTS, ...over });
const hasEp = (row: DiscoveredEndpoint, s: FilterState) => buildPredicate(E_COLS, s, 0)(row);

describe('the sweep-results filter row', () => {
  it('shows everything when nothing is set', () => {
    expect(hasCand(cand(), C_DEFAULTS)).toBe(true);
    expect(hasCand(cand({ reachable: false }), C_DEFAULTS)).toBe(true);
    expect(isAnyFiltered(C_COLS, C_DEFAULTS)).toBe(false);
  });

  it('says which answers to keep, in both directions', () => {
    // The checkbox could only say "hide the silent ones". "Only the silent ones" — the addresses
    // worth chasing after a sweep, because something is there and did not answer — was unsayable.
    expect(hasCand(cand({ reachable: false }), cf({ reach: 'reachable' }))).toBe(false);
    expect(hasCand(cand(), cf({ reach: 'reachable' }))).toBe(true);
    expect(hasCand(cand({ reachable: false }), cf({ reach: 'silent' }))).toBe(true);
    expect(hasCand(cand(), cf({ reach: 'silent' }))).toBe(false);
    // Both ticked is the same as neither, never "nothing matches".
    expect(hasCand(cand(), cf({ reach: 'reachable,silent' }))).toBe(true);
  });

  it('asks the address and the identity separately, where the box asked both at once', () => {
    expect(hasCand(cand(), cf({ address: '192.168.1.1' }))).toBe(true);
    expect(hasCand(cand(), cf({ identity: 'CISCO' }))).toBe(true);
    expect(hasCand(cand(), cf({ identity: 'isr4331' }))).toBe(true);
    // The distinction the one box could not draw: this is an address, not an identity.
    expect(hasCand(cand(), cf({ identity: '192.168' }))).toBe(false);
    expect(hasCand(cand(), cf({ identity: 'juniper' }))).toBe(false);
  });

  it('reads all four identity strings, because the cell renders all four', () => {
    // sysname, sysdescr, vendor and model are stacked in one cell — a term visible on screen has to
    // find its row whichever line it is on.
    expect(hasCand(cand(), cf({ identity: 'rtr-01' }))).toBe(true);
    expect(hasCand(cand(), cf({ identity: 'IOS Software' }))).toBe(true);
  });

  it('survives a candidate that answered nothing at all', () => {
    // An unreachable address has null everywhere but the address, and a filter must not crash on it.
    const silent = cand({
      reachable: false,
      sysname: null,
      sysdescr: null,
      vendor: null,
      model: null,
    });
    expect(hasCand(silent, cf({ address: '192.168' }))).toBe(true);
    expect(hasCand(silent, cf({ identity: 'cisco' }))).toBe(false);
  });

  it('labels every column, so no cell shows a raw key', () => {
    const labels = candidateLabels(t);
    for (const c of C_COLS) expect(labels[c.key]).toBeTruthy();
  });

  it('flips isAnyFiltered for every column', () => {
    for (const c of C_COLS) {
      expect(isAnyFiltered(C_COLS, cf({ [c.key]: c.key === 'reach' ? 'silent' : 'x' }))).toBe(true);
    }
  });
});

describe('the seen-endpoints filter row', () => {
  it('hides already-imported endpoints by default, which is what the table always did', () => {
    // The default is deliberately not "everything": the table has always hidden promoted rows,
    // and preserving that keeps the behaviour operators know while making it visible as a control.
    expect(E_DEFAULTS.monitored).toBe('unmonitored');
    expect(hasEp(ep(), E_DEFAULTS)).toBe(true);
    expect(hasEp(ep({ promoted_node_id: 'n1' }), E_DEFAULTS)).toBe(false);
  });

  it('shows the imported ones, or only them', () => {
    // The improvement: an endpoint that vanished because someone else imported it used to be
    // indistinguishable from one that stopped being seen. And "which of these did we already take"
    // is now its own question rather than the absence of a filter.
    expect(hasEp(ep({ promoted_node_id: 'n1' }), ef({ monitored: '' }))).toBe(true);
    expect(hasEp(ep({ promoted_node_id: 'n1' }), ef({ monitored: 'monitored' }))).toBe(true);
    expect(hasEp(ep(), ef({ monitored: 'monitored' }))).toBe(false);
  });

  it('asks the address, the MAC and the neighbour separately', () => {
    expect(hasEp(ep(), ef({ ip: '192.168.1.5' }))).toBe(true);
    expect(hasEp(ep(), ef({ mac: 'AA:BB' }))).toBe(true);
    expect(hasEp(ep(), ef({ via: 'sw-core' }))).toBe(true);
    // The distinction the one box could not draw: this is the neighbour, not the address.
    expect(hasEp(ep(), ef({ ip: 'sw-core' }))).toBe(false);
    expect(hasEp(ep(), ef({ via: 'sw-edge' }))).toBe(false);
  });

  it('labels every column, so no cell shows a raw key', () => {
    const labels = endpointLabels(t);
    for (const c of E_COLS) expect(labels[c.key]).toBeTruthy();
  });

  it('reports the default view as filtered, because it is', () => {
    // ⚠️ Deliberate, and the reason the page always shows the total beside the count: the default
    // hides rows, so claiming "nothing is narrowing this" would be the untrue half of the pair.
    expect(isAnyFiltered(E_COLS, E_DEFAULTS)).toBe(true);
    expect(isAnyFiltered(E_COLS, defaultFilters(E_COLS))).toBe(false);
  });
});
