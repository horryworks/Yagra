// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Active-alerts triage filter (no DOM — Vitest node env).

import { describe, expect, it, vi } from 'vitest';
import { SEVERITIES, type NodeState } from '../types/api';
import { SEVERITY_ORDER } from '../lib/nodeState';
import {
  ACK_FILTERS,
  ACK_STATES,
  DEFAULT_FILTERS,
  isFiltered,
  matchesActiveAlert,
  readFilters,
  writeFilters,
  type ActiveAlertFilters,
  type FilterableAlert,
} from './activeAlertFilters';

const read = (qs: string) =>
  readFilters(new URLSearchParams(qs), SEVERITIES, SEVERITY_ORDER as readonly NodeState[]);

/** A node alert. `nameOf` below is what turns the id into `rtr-01`. */
const nodeAlert = (over: Partial<FilterableAlert> = {}): FilterableAlert => ({
  node: 'aaaaaaaa-0000-0000-0000-000000000001',
  subject_kind: 'node',
  severity: 'critical',
  state: 'unreachable',
  metric: 'icmp_rtt_ms',
  ...over,
});

const poolAlert = (over: Partial<FilterableAlert> = {}): FilterableAlert => ({
  node: 'pool:tokyo',
  subject_kind: 'pool',
  subject_name: 'tokyo',
  severity: 'warning',
  state: 'unknown',
  metric: 'pool_coverage',
  ...over,
});

const NAMES: Record<string, string> = {
  'aaaaaaaa-0000-0000-0000-000000000001': 'rtr-01',
};
const nameOf = (id: string) => NAMES[id] ?? id;

const withFilters = (over: Partial<ActiveAlertFilters>): ActiveAlertFilters => ({
  ...DEFAULT_FILTERS,
  ...over,
});

describe('the ack vocabulary', () => {
  it('offers the no-filter option exactly once', () => {
    // The dropdown maps ACK_STATES and lets FilterSelect supply ''. If '' also appeared in
    // ACK_STATES the operator would see two "no filter" rows that do the same thing.
    expect(ACK_STATES).not.toContain('');
    expect(ACK_FILTERS.filter((v) => v === '')).toHaveLength(1);
    expect(ACK_FILTERS).toEqual(['', ...ACK_STATES]);
  });
});

describe('matchesActiveAlert', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesActiveAlert(nodeAlert(), DEFAULT_FILTERS, nameOf)).toBe(true);
    expect(matchesActiveAlert(poolAlert(), DEFAULT_FILTERS, nameOf)).toBe(true);
  });

  it('never resolves a name unless a term was typed', () => {
    // Load-bearing, not a micro-optimization: `nameOf` is useEntityNames' lazy resolver and asking
    // it about an id enqueues that id for the next batch request. A predicate that asked
    // unconditionally would resolve every node in an outage in order to draw thirty rows.
    const spy = vi.fn(nameOf);
    matchesActiveAlert(nodeAlert(), withFilters({ severity: 'critical' }), spy);
    expect(spy).not.toHaveBeenCalled();
    matchesActiveAlert(nodeAlert(), withFilters({ q: 'rtr' }), spy);
    expect(spy).toHaveBeenCalledWith('aaaaaaaa-0000-0000-0000-000000000001');
  });

  it('filters by severity and by state independently', () => {
    const a = nodeAlert({ severity: 'warning', state: 'warning' });
    expect(matchesActiveAlert(a, withFilters({ severity: 'warning' }), nameOf)).toBe(true);
    expect(matchesActiveAlert(a, withFilters({ severity: 'critical' }), nameOf)).toBe(false);
    expect(matchesActiveAlert(a, withFilters({ state: 'warning' }), nameOf)).toBe(true);
    expect(matchesActiveAlert(a, withFilters({ state: 'unreachable' }), nameOf)).toBe(false);
  });

  it('splits acked from not-acked, and each excludes the other', () => {
    // The mirror-image assertion is the point: "Acked" showing un-acked rows was the History bug
    // this whole pass shipped a fix for, and it looked plausible from one direction only.
    const acked = nodeAlert({ acked: { source: 'pagerduty', by: 'oncall', at_unix_ms: 1 } });
    const plain = nodeAlert();
    expect(matchesActiveAlert(acked, withFilters({ ack: 'acked' }), nameOf)).toBe(true);
    expect(matchesActiveAlert(plain, withFilters({ ack: 'acked' }), nameOf)).toBe(false);
    expect(matchesActiveAlert(plain, withFilters({ ack: 'unacked' }), nameOf)).toBe(true);
    expect(matchesActiveAlert(acked, withFilters({ ack: 'unacked' }), nameOf)).toBe(false);
    // And neither is applied at all when the filter is unset.
    expect(matchesActiveAlert(acked, DEFAULT_FILTERS, nameOf)).toBe(true);
    expect(matchesActiveAlert(plain, DEFAULT_FILTERS, nameOf)).toBe(true);
  });

  it('searches the resolved node name, the raw id and the metric', () => {
    const a = nodeAlert();
    expect(matchesActiveAlert(a, withFilters({ q: 'rtr-0' }), nameOf)).toBe(true);
    // The id is the handle in a deep link and in an API error, so pasting one must find its row.
    expect(matchesActiveAlert(a, withFilters({ q: '0000000001' }), nameOf)).toBe(true);
    expect(matchesActiveAlert(a, withFilters({ q: 'icmp' }), nameOf)).toBe(true);
    expect(matchesActiveAlert(a, withFilters({ q: 'sw-04' }), nameOf)).toBe(false);
  });

  it('searches case-insensitively and ignores surrounding space', () => {
    const a = nodeAlert();
    expect(matchesActiveAlert(a, withFilters({ q: '  RTR-01 ' }), nameOf)).toBe(true);
    // A term of nothing but spaces is not a filter.
    expect(matchesActiveAlert(a, withFilters({ q: '   ' }), nameOf)).toBe(true);
  });

  it('matches a pool alert on its pool name, never through the resolver', () => {
    // `node` is `pool:tokyo` here, not a UUID — handing that to a name resolver is the mistake
    // `lib/alertSubject` exists to prevent.
    const spy = vi.fn(nameOf);
    expect(matchesActiveAlert(poolAlert(), withFilters({ q: 'tokyo' }), spy)).toBe(true);
    expect(spy).not.toHaveBeenCalled();
    expect(matchesActiveAlert(poolAlert(), withFilters({ q: 'osaka' }), nameOf)).toBe(false);
  });

  it('applies every set filter together, not the first that matches', () => {
    const a = nodeAlert({ severity: 'warning' });
    expect(
      matchesActiveAlert(a, withFilters({ severity: 'warning', q: 'icmp' }), nameOf),
    ).toBe(true);
    expect(
      matchesActiveAlert(a, withFilters({ severity: 'critical', q: 'icmp' }), nameOf),
    ).toBe(false);
  });
});

describe('isFiltered', () => {
  it('is false for the default view', () => {
    expect(isFiltered(DEFAULT_FILTERS)).toBe(false);
  });

  it('flips for every field, so a new filter cannot be forgotten here', () => {
    // Driven off the defaults rather than a hand-written list: a filter added without its clause
    // would otherwise make the screen say "there is nothing here" while a filter hides the rows.
    const sample: ActiveAlertFilters = {
      severity: 'critical',
      state: 'unreachable',
      ack: 'acked',
      q: 'rtr',
    };
    for (const key of Object.keys(DEFAULT_FILTERS) as (keyof ActiveAlertFilters)[]) {
      expect(isFiltered(withFilters({ [key]: sample[key] }))).toBe(true);
    }
  });
});

describe('the URL codec', () => {
  it('reads an empty query as the default view', () => {
    expect(read('')).toEqual(DEFAULT_FILTERS);
  });

  it('round-trips every field', () => {
    const f: ActiveAlertFilters = {
      severity: 'warning',
      state: 'maintenance',
      ack: 'unacked',
      q: 'rtr-01',
    };
    const params = new URLSearchParams();
    writeFilters(params, f);
    expect(read(params.toString())).toEqual(f);
  });

  it('leaves no query string at all for the default view', () => {
    const params = new URLSearchParams();
    writeFilters(params, DEFAULT_FILTERS);
    expect(params.toString()).toBe('');
  });

  it('clears a key when its filter goes back to the default', () => {
    const params = new URLSearchParams('severity=critical&q=rtr&ack=acked');
    writeFilters(params, DEFAULT_FILTERS);
    expect(params.toString()).toBe('');
  });

  it('falls back instead of rejecting an unknown value', () => {
    // A stale bookmark or a hand-edited URL must not render a control whose value is not one of its
    // own options — the opposite of the API edge, where an unknown token is a 400.
    expect(read('severity=nuclear&state=melted&ack=maybe')).toEqual(DEFAULT_FILTERS);
  });

  it('trims the search term and drops one that is only space', () => {
    expect(read('q=%20%20rtr%20%20').q).toBe('rtr');
    const params = new URLSearchParams();
    writeFilters(params, withFilters({ q: '   ' }));
    expect(params.toString()).toBe('');
  });

  it('leaves query keys it does not own alone', () => {
    // The screen shares its URL with anything else that parks state there.
    const params = new URLSearchParams('tab=map&severity=info');
    writeFilters(params, DEFAULT_FILTERS);
    expect(params.toString()).toBe('tab=map');
  });
});
