// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the three node-detail tab filters (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { Neighbor, NodeMetricEntry } from '../../types/api';
import {
  DEFAULT_INTERFACE_FILTERS,
  DEFAULT_METRIC_FILTERS,
  DEFAULT_NEIGHBOR_FILTERS,
  IF_STATES,
  ifState,
  isInterfaceFiltered,
  isMetricFiltered,
  isNeighborFiltered,
  matchesInterface,
  matchesMetric,
  matchesNeighbor,
  metricIsFlowing,
  type FilterableInterface,
  type InterfaceFilters,
  type MetricFilters,
  type NeighborFilters,
} from './tabFilters';

const iface = (over: Partial<FilterableInterface> = {}): FilterableInterface => ({
  ifindex: 3,
  if_name: 'GigabitEthernet0/1',
  if_alias: 'uplink to core',
  oper_status: 1,
  ...over,
});

const nb = (over: Partial<Neighbor> = {}): Neighbor => ({
  proto: 'lldp',
  local_ifindex: 3,
  local_port: 'Gi0/1',
  remote_port: 'Te1/1/1',
  remote_port_desc: 'to access switch',
  remote_sys_name: 'sw-core',
  remote_sys_desc: 'Cisco IOS',
  remote_chassis: '',
  remote_mgmt_addr: null,
  remote_platform: null,
  capabilities: [],
  ...over,
});

const metric = (over: Partial<NodeMetricEntry> = {}): NodeMetricEntry => ({
  metric: 'icmp_rtt_ms',
  metric_kind: 'gauge',
  dimension: 'none',
  status: 'ok',
  series_count: 1,
  ...over,
});

const iff = (over: Partial<InterfaceFilters>): InterfaceFilters => ({
  ...DEFAULT_INTERFACE_FILTERS,
  ...over,
});
const nf = (over: Partial<NeighborFilters>): NeighborFilters => ({
  ...DEFAULT_NEIGHBOR_FILTERS,
  ...over,
});
const mf = (over: Partial<MetricFilters>): MetricFilters => ({ ...DEFAULT_METRIC_FILTERS, ...over });

describe('ifState', () => {
  it('treats only 1 as up, and a missing answer as unknown', () => {
    // `oper_status` is the SNMP ifOperStatus integer. Anything that is not 1 is not up — and null
    // means the poller has never had an answer, which is not the same as down.
    expect(ifState(1)).toBe('up');
    expect(ifState(2)).toBe('down');
    expect(ifState(7)).toBe('down');
    expect(ifState(null)).toBe('unknown');
    expect(ifState(undefined)).toBe('unknown');
  });

  it('buckets exhaustively, so the three options are the whole list', () => {
    const seen = new Set([ifState(1), ifState(2), ifState(null)]);
    expect([...seen].sort()).toEqual([...IF_STATES].sort());
  });
});

describe('matchesInterface', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesInterface(iface(), DEFAULT_INTERFACE_FILTERS)).toBe(true);
  });

  it('filters by running state', () => {
    expect(matchesInterface(iface(), iff({ state: 'up' }))).toBe(true);
    expect(matchesInterface(iface(), iff({ state: 'down' }))).toBe(false);
    expect(matchesInterface(iface({ oper_status: 2 }), iff({ state: 'down' }))).toBe(true);
    expect(matchesInterface(iface({ oper_status: null }), iff({ state: 'unknown' }))).toBe(true);
  });

  it('searches the name and the description', () => {
    expect(matchesInterface(iface(), iff({ q: 'GIGABIT' }))).toBe(true);
    expect(matchesInterface(iface(), iff({ q: 'uplink' }))).toBe(true);
    expect(matchesInterface(iface(), iff({ q: 'downlink' }))).toBe(false);
  });

  it('finds a nameless interface by the label the row actually shows', () => {
    // The row falls back to `if<ifindex>`, so typing what is on screen has to match.
    expect(matchesInterface(iface({ if_name: null }), iff({ q: 'if3' }))).toBe(true);
  });

  it('flips isFiltered for every field', () => {
    expect(isInterfaceFiltered(DEFAULT_INTERFACE_FILTERS)).toBe(false);
    expect(isInterfaceFiltered(iff({ state: 'down' }))).toBe(true);
    expect(isInterfaceFiltered(iff({ q: 'x' }))).toBe(true);
  });
});

describe('matchesNeighbor', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesNeighbor(nb(), DEFAULT_NEIGHBOR_FILTERS)).toBe(true);
  });

  it('filters by the protocol that reported the adjacency', () => {
    // A switch running both reports the same physical link twice with different identities, so
    // being able to look at one protocol's view alone is the point.
    expect(matchesNeighbor(nb(), nf({ proto: 'lldp' }))).toBe(true);
    expect(matchesNeighbor(nb(), nf({ proto: 'cdp' }))).toBe(false);
  });

  it('searches the peer and both ends of the link', () => {
    expect(matchesNeighbor(nb(), nf({ q: 'SW-CORE' }))).toBe(true);
    expect(matchesNeighbor(nb(), nf({ q: 'gi0/1' }))).toBe(true);
    expect(matchesNeighbor(nb(), nf({ q: 'te1/1/1' }))).toBe(true);
    expect(matchesNeighbor(nb(), nf({ q: 'sw-edge' }))).toBe(false);
  });

  it('survives a neighbour that reported almost nothing', () => {
    const bare = nb({ remote_sys_name: null, remote_port: '', remote_port_desc: null });
    expect(matchesNeighbor(bare, nf({ q: 'gi0/1' }))).toBe(true);
    expect(matchesNeighbor(bare, nf({ q: 'sw-core' }))).toBe(false);
  });

  it('flips isFiltered for every field', () => {
    expect(isNeighborFiltered(DEFAULT_NEIGHBOR_FILTERS)).toBe(false);
    expect(isNeighborFiltered(nf({ proto: 'cdp' }))).toBe(true);
    expect(isNeighborFiltered(nf({ q: 'x' }))).toBe(true);
  });
});

describe('matchesMetric', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesMetric(metric(), DEFAULT_METRIC_FILTERS)).toBe(true);
    expect(matchesMetric(metric({ status: 'no_data' }), DEFAULT_METRIC_FILTERS)).toBe(true);
  });

  it('hides only the ones that are not arriving', () => {
    // `no_data` is the one state that means "nothing is coming".
    expect(matchesMetric(metric({ status: 'no_data' }), mf({ flowingOnly: true }))).toBe(false);
    expect(matchesMetric(metric(), mf({ flowingOnly: true }))).toBe(true);
  });

  it('keeps an unconfigured metric, because unconfigured means arriving without a collection set', () => {
    // ⚠️ This asserted the opposite and was wrong. `MetricStatus` crosses two facts: `ok` is
    // configured AND arriving, `unconfigured` is arriving with NO collection set behind it —
    // reachability, `http_up`, `dns_up`, the neighbour count, JSON-extracted values.
    //
    // The consequence was worst exactly where the tab has least: a URL or DNS monitor has no
    // collection set at all, so every metric it has is `unconfigured`, and "only the ones that are
    // flowing" emptied the list it was meant to narrow.
    expect(matchesMetric(metric({ status: 'unconfigured' }), mf({ flowingOnly: true }))).toBe(true);
    expect(
      matchesMetric(metric({ metric: 'http_up', status: 'unconfigured' }), mf({ flowingOnly: true })),
    ).toBe(true);
  });

  it('classifies every status the API can return', () => {
    // Driven off the union so a new status has to be decided rather than falling into "not
    // flowing" by default — which is the direction that hides rows.
    const seen = (['ok', 'no_data', 'unconfigured'] as const).map((status) => [
      status,
      metricIsFlowing(metric({ status })),
    ]);
    expect(seen).toEqual([
      ['ok', true],
      ['no_data', false],
      ['unconfigured', true],
    ]);
  });

  it('searches the metric name', () => {
    expect(matchesMetric(metric(), mf({ q: 'ICMP' }))).toBe(true);
    expect(matchesMetric(metric(), mf({ q: 'cpu' }))).toBe(false);
  });

  it('flips isFiltered for every field', () => {
    expect(isMetricFiltered(DEFAULT_METRIC_FILTERS)).toBe(false);
    expect(isMetricFiltered(mf({ flowingOnly: true }))).toBe(true);
    expect(isMetricFiltered(mf({ q: 'x' }))).toBe(true);
  });
});
