// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Nodes ▸ Discovery filters (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { DiscoveredEndpoint, DiscoveryCandidate } from '../types/api';
import {
  DEFAULT_CANDIDATE_FILTERS,
  DEFAULT_ENDPOINT_FILTERS,
  isCandidateFiltered,
  isEndpointFiltered,
  matchesCandidate,
  matchesEndpoint,
  type CandidateFilters,
  type EndpointFilters,
} from './discoveryFilters';

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

const cf = (over: Partial<CandidateFilters>): CandidateFilters => ({
  ...DEFAULT_CANDIDATE_FILTERS,
  ...over,
});
const ef = (over: Partial<EndpointFilters>): EndpointFilters => ({
  ...DEFAULT_ENDPOINT_FILTERS,
  ...over,
});

describe('matchesCandidate', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesCandidate(cand(), DEFAULT_CANDIDATE_FILTERS)).toBe(true);
    expect(matchesCandidate(cand({ reachable: false }), DEFAULT_CANDIDATE_FILTERS)).toBe(true);
  });

  it('hides the silent addresses on request — the bulk of a wide sweep', () => {
    expect(matchesCandidate(cand({ reachable: false }), cf({ reachableOnly: true }))).toBe(false);
    expect(matchesCandidate(cand(), cf({ reachableOnly: true }))).toBe(true);
  });

  it('searches the address and everything the device said it is', () => {
    expect(matchesCandidate(cand(), cf({ q: '192.168.1.1' }))).toBe(true);
    expect(matchesCandidate(cand(), cf({ q: 'CISCO' }))).toBe(true);
    expect(matchesCandidate(cand(), cf({ q: 'isr4331' }))).toBe(true);
    expect(matchesCandidate(cand(), cf({ q: 'juniper' }))).toBe(false);
  });

  it('survives a candidate that answered nothing at all', () => {
    // An unreachable address has null everywhere but the address, and a filter must not crash on it.
    const silent = cand({ reachable: false, sysname: null, sysdescr: null, vendor: null, model: null });
    expect(matchesCandidate(silent, cf({ q: '192.168' }))).toBe(true);
    expect(matchesCandidate(silent, cf({ q: 'cisco' }))).toBe(false);
  });

  it('flips isFiltered for every field', () => {
    expect(isCandidateFiltered(DEFAULT_CANDIDATE_FILTERS)).toBe(false);
    expect(isCandidateFiltered(cf({ reachableOnly: true }))).toBe(true);
    expect(isCandidateFiltered(cf({ q: 'x' }))).toBe(true);
  });
});

describe('matchesEndpoint', () => {
  it('hides already-imported endpoints by default, which is what the table always did', () => {
    // The default is deliberately not "everything": the table has always hidden promoted rows,
    // and preserving that keeps the behaviour operators know while making it visible as a control.
    expect(DEFAULT_ENDPOINT_FILTERS.unmonitoredOnly).toBe(true);
    expect(matchesEndpoint(ep(), DEFAULT_ENDPOINT_FILTERS)).toBe(true);
    expect(matchesEndpoint(ep({ promoted_node_id: 'n1' }), DEFAULT_ENDPOINT_FILTERS)).toBe(false);
  });

  it('shows the imported ones once the toggle is turned off', () => {
    // The improvement: an endpoint that vanished because someone else imported it used to be
    // indistinguishable from one that stopped being seen.
    expect(matchesEndpoint(ep({ promoted_node_id: 'n1' }), ef({ unmonitoredOnly: false }))).toBe(true);
  });

  it('searches the address, the MAC and the neighbour it was seen through', () => {
    expect(matchesEndpoint(ep(), ef({ q: '192.168.1.5' }))).toBe(true);
    expect(matchesEndpoint(ep(), ef({ q: 'AA:BB' }))).toBe(true);
    expect(matchesEndpoint(ep(), ef({ q: 'sw-core' }))).toBe(true);
    expect(matchesEndpoint(ep(), ef({ q: 'sw-edge' }))).toBe(false);
  });

  it('flips isFiltered for every field', () => {
    expect(isEndpointFiltered(DEFAULT_ENDPOINT_FILTERS)).toBe(false);
    expect(isEndpointFiltered(ef({ unmonitoredOnly: false }))).toBe(true);
    expect(isEndpointFiltered(ef({ q: 'x' }))).toBe(true);
  });
});
