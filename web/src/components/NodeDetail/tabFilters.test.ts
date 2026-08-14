// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the three node-detail tab filters (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { Neighbor, NodeMetricEntry } from '../../types/api';
import {
  IF_STATES,
  ifState,
  interfaceColumns,
  metricColumns,
  metricIsFlowing,
  neighborFilters,
  type FilterableInterface,
} from './tabFilters';
import {
  defaultFilters,
  isAnyFiltered,
  type FilterState,
  type FilterableColumn,
} from '../../lib/columnFilter';
import { matchesFilters } from '../../lib/filterPredicate';

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

const t = ((k: string) => k) as unknown as Parameters<typeof neighborFilters>[0];
const NOW = Date.parse('2026-08-13T12:00:00Z');

const IF_COLS = interfaceColumns(t);
const IF_DEFAULTS = defaultFilters(IF_COLS);
const iff = (over: FilterState): FilterState => ({ ...IF_DEFAULTS, ...over });
const hasIf = (row: FilterableInterface, state: FilterState) =>
  matchesFilters(row, IF_COLS, state, NOW);

const M_COLS = metricColumns(t);
const M_DEFAULTS = defaultFilters(M_COLS);
const mf = (over: FilterState): FilterState => ({ ...M_DEFAULTS, ...over });
const hasMetric = (row: NodeMetricEntry, state: FilterState) =>
  matchesFilters(row, M_COLS, state, NOW);

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

describe('the interfaces filter row', () => {
  it('shows everything when nothing is set', () => {
    expect(hasIf(iface(), IF_DEFAULTS)).toBe(true);
    expect(isAnyFiltered(IF_COLS, IF_DEFAULTS)).toBe(false);
  });

  it('filters by running state', () => {
    expect(hasIf(iface(), iff({ oper: 'up' }))).toBe(true);
    expect(hasIf(iface(), iff({ oper: 'down' }))).toBe(false);
    expect(hasIf(iface({ oper_status: 2 }), iff({ oper: 'down' }))).toBe(true);
    expect(hasIf(iface({ oper_status: null }), iff({ oper: 'unknown' }))).toBe(true);
    // …and several at once, which the dropdown could not say: "anything not up" is two buckets.
    expect(hasIf(iface({ oper_status: null }), iff({ oper: 'down,unknown' }))).toBe(true);
    expect(hasIf(iface(), iff({ oper: 'down,unknown' }))).toBe(false);
  });

  it('asks the name and the description separately, where the box asked both at once', () => {
    expect(hasIf(iface(), iff({ if_name: 'GIGABIT' }))).toBe(true);
    expect(hasIf(iface(), iff({ if_alias: 'uplink' }))).toBe(true);
    // The distinction the one box could not draw: this text is in the *description*, not the name.
    expect(hasIf(iface(), iff({ if_name: 'uplink' }))).toBe(false);
    expect(hasIf(iface(), iff({ if_alias: 'downlink' }))).toBe(false);
  });

  it('finds a nameless interface by the label the row actually shows', () => {
    // The row falls back to `if<ifindex>`, so typing what is on screen has to match.
    expect(hasIf(iface({ if_name: null }), iff({ if_name: 'if3' }))).toBe(true);
  });

  it('flips isAnyFiltered for every column', () => {
    for (const c of IF_COLS) {
      expect(isAnyFiltered(IF_COLS, iff({ [c.key]: c.key === 'oper' ? 'up' : 'x' }))).toBe(true);
    }
  });
});

describe('the neighbours filter row', () => {
  const NB_COLS: FilterableColumn<Neighbor>[] = Object.entries(neighborFilters(t)).map(
    ([key, filter]) => ({ key, filter }),
  );
  const NB_DEFAULTS = defaultFilters(NB_COLS);
  const nbf = (over: Record<string, string>): FilterState => ({ ...NB_DEFAULTS, ...over });
  const has = (row: Neighbor, state: FilterState) => matchesFilters(row, NB_COLS, state, NOW);

  it('shows everything when nothing is set', () => {
    expect(has(nb(), NB_DEFAULTS)).toBe(true);
    expect(isAnyFiltered(NB_COLS, NB_DEFAULTS)).toBe(false);
  });

  it('filters by the protocol that reported the adjacency', () => {
    // A switch running both reports the same physical link twice with different identities, so
    // being able to look at one protocol's view alone is the point.
    expect(has(nb(), nbf({ proto: 'lldp' }))).toBe(true);
    expect(has(nb(), nbf({ proto: 'cdp' }))).toBe(false);
    // …and both together, which the single-choice dropdown could not express.
    expect(has(nb(), nbf({ proto: 'cdp,lldp' }))).toBe(true);
  });

  it('asks each end of the link separately, where the search box asked all four at once', () => {
    expect(has(nb(), nbf({ peer: 'SW-CORE' }))).toBe(true);
    expect(has(nb(), nbf({ local: 'gi0/1' }))).toBe(true);
    expect(has(nb(), nbf({ remote_port: 'te1/1/1' }))).toBe(true);
    // The distinction the one box could not draw: this port number is the *local* one.
    expect(has(nb(), nbf({ remote_port: 'gi0/1' }))).toBe(false);
    expect(has(nb(), nbf({ peer: 'sw-edge' }))).toBe(false);
  });

  it('matches the peer column on whichever identity the cell is showing', () => {
    // ⚠️ The cell renders the system name, or the chassis id when there is no name. Reading only
    // `remote_sys_name` would mean typing what is on screen finds nothing for a bare neighbour.
    const bare = nb({ remote_sys_name: null, remote_port: '', remote_port_desc: null });
    expect(has(bare, nbf({ peer: bare.remote_chassis }))).toBe(true);
    expect(has(bare, nbf({ peer: 'sw-core' }))).toBe(false);
    expect(has(bare, nbf({ local: 'gi0/1' }))).toBe(true);
  });

  it('flips isAnyFiltered for every column', () => {
    for (const key of Object.keys(NB_DEFAULTS)) {
      expect(isAnyFiltered(NB_COLS, nbf({ [key]: 'x' }))).toBe(true);
    }
  });
});

describe('the collection filter row', () => {
  it('shows everything when nothing is set', () => {
    expect(hasMetric(metric(), M_DEFAULTS)).toBe(true);
    expect(hasMetric(metric({ status: 'no_data' }), M_DEFAULTS)).toBe(true);
    expect(isAnyFiltered(M_COLS, M_DEFAULTS)).toBe(false);
  });

  it('says which statuses to keep, where the checkbox could only say "hide the silent ones"', () => {
    // ⚠️ **The widening is the point.** "Show me only the configured-but-silent ones" is the
    // question an operator asks when a collection set has stopped working, and it was unsayable:
    // the checkbox's only two positions were "everything" and "everything except no_data".
    expect(hasMetric(metric({ status: 'no_data' }), mf({ status: 'no_data' }))).toBe(true);
    expect(hasMetric(metric(), mf({ status: 'no_data' }))).toBe(false);
    // The old checkbox's meaning, still expressible — as the two statuses that mean "arriving".
    const flowing = mf({ status: 'ok,unconfigured' });
    expect(hasMetric(metric({ status: 'no_data' }), flowing)).toBe(false);
    expect(hasMetric(metric(), flowing)).toBe(true);
  });

  it('offers unconfigured as its own choice, because unconfigured means arriving', () => {
    // ⚠️ A test here once asserted the opposite and was wrong. `MetricStatus` crosses two facts:
    // `ok` is configured AND arriving, `unconfigured` is arriving with NO collection set behind it
    // — reachability, `http_up`, `dns_up`, the neighbour count, JSON-extracted values.
    //
    // The consequence was worst exactly where the tab has least: a URL or DNS monitor has no
    // collection set at all, so every metric it has is `unconfigured`, and "only the ones that are
    // flowing" emptied the list it was meant to narrow. Splitting the control into the three real
    // statuses removes the reading that made that possible.
    const status = M_COLS.find((c) => c.key === 'status')?.filter;
    expect(status?.kind === 'enum' && status.options.map((o) => o.value)).toEqual([
      'ok',
      'no_data',
      'unconfigured',
    ]);
    expect(
      hasMetric(metric({ metric: 'http_up', status: 'unconfigured' }), mf({ status: 'unconfigured' })),
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
    expect(hasMetric(metric(), mf({ metric: 'ICMP' }))).toBe(true);
    expect(hasMetric(metric(), mf({ metric: 'cpu' }))).toBe(false);
    // NOT, which the plain box did not offer: "everything that is not an interface counter".
    expect(hasMetric(metric({ metric: 'if_hc_in_octets' }), mf({ metric: '!if_' }))).toBe(false);
    expect(hasMetric(metric(), mf({ metric: '!if_' }))).toBe(true);
  });

  it('flips isAnyFiltered for every column', () => {
    for (const c of M_COLS) {
      expect(isAnyFiltered(M_COLS, mf({ [c.key]: c.key === 'status' ? 'ok' : 'x' }))).toBe(true);
    }
  });
});
