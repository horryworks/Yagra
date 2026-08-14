// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Active-alerts triage filter (no DOM — Vitest node env).
//
// Rewritten for ADR-053 Inc.6, which moved this screen onto the shared column-filter model. Every
// property the hand-written predicate was tested for is still asserted here — the interesting one
// being that a name is never resolved unless a term was typed, which used to be an explicit early
// return and is now a consequence of `buildPredicate` not compiling an inactive column.

import { describe, expect, it, vi } from 'vitest';
import { SEVERITIES, type NodeState } from '../types/api';
import { SEVERITY_ORDER } from '../lib/nodeState';
import { defaultFilters, isAnyFiltered, type FilterState } from '../lib/columnFilter';
import {
  ACK_STATES,
  activeAlertColumns,
  activeAlertLabels,
  alertPredicate,
  readFilters,
  writeFilters,
  type FilterableAlert,
  type NameOf,
} from './activeAlertFilters';

const NAMES: Record<string, string> = {
  'aaaaaaaa-0000-0000-0000-000000000001': 'rtr-01',
};
const nameOf: NameOf = (id) => NAMES[id] ?? id;

/** `t` is only ever used for labels here, so echoing the key is enough and keeps i18n out. */
const t = ((k: string) => k) as unknown as Parameters<typeof activeAlertColumns>[0];

const cols = (resolver: NameOf = nameOf) =>
  activeAlertColumns(t, resolver, SEVERITIES, SEVERITY_ORDER as readonly NodeState[]);

const DEFAULTS = defaultFilters(cols());

const withFilters = (over: FilterState): FilterState => ({ ...DEFAULTS, ...over });

const matches = (a: FilterableAlert, f: FilterState, resolver: NameOf = nameOf) =>
  alertPredicate(cols(resolver), f)(a);

/** A node alert. `nameOf` above is what turns the id into `rtr-01`. */
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

const read = (qs: string) => readFilters(cols(), new URLSearchParams(qs));

describe('the ack vocabulary', () => {
  it('offers no empty option, because "no filter" is the empty set', () => {
    // Under the old single-valued dropdown this had to be checked against a second `ACK_FILTERS`
    // list that added `''`. A multi-select has no such option to duplicate: nothing ticked *is* the
    // unfiltered state, so there is one spelling of it and the second list is gone.
    expect(ACK_STATES).not.toContain('');
    expect(DEFAULTS.ack).toBe('');
  });
});

describe('the column set', () => {
  it('labels every column, so the bar never shows a raw key', () => {
    // The bar has no header above each control, so a missing label is the only thing between the
    // operator and a key like `q`. Nothing type-checks the `labels[c.key]` lookup.
    const labels = activeAlertLabels(t);
    for (const c of cols()) expect(labels[c.key]).toBeTruthy();
  });
});

describe('the predicate', () => {
  it('shows everything when nothing is set', () => {
    expect(matches(nodeAlert(), DEFAULTS)).toBe(true);
    expect(matches(poolAlert(), DEFAULTS)).toBe(true);
  });

  it('never resolves a name unless a term was typed', () => {
    // Load-bearing, not a micro-optimization: `nameOf` is useEntityNames' lazy resolver and asking
    // it about an id enqueues that id for the next batch request. A predicate that asked
    // unconditionally would resolve every node in an outage in order to draw thirty rows.
    //
    // This used to be an explicit `if (q === '') return true`. It now falls out of `buildPredicate`
    // compiling a column that is not narrowing to `null` — which is why the assertion moved but did
    // not weaken.
    const spy = vi.fn(nameOf);
    matches(nodeAlert(), withFilters({ severity: 'critical' }), spy);
    expect(spy).not.toHaveBeenCalled();
    matches(nodeAlert(), withFilters({ q: 'rtr' }), spy);
    expect(spy).toHaveBeenCalledWith('aaaaaaaa-0000-0000-0000-000000000001');
  });

  it('filters by severity and by state independently', () => {
    const a = nodeAlert({ severity: 'warning', state: 'warning' });
    expect(matches(a, withFilters({ severity: 'warning' }))).toBe(true);
    expect(matches(a, withFilters({ severity: 'critical' }))).toBe(false);
    expect(matches(a, withFilters({ state: 'warning' }))).toBe(true);
    expect(matches(a, withFilters({ state: 'unreachable' }))).toBe(false);
  });

  it('takes several values in one column — the thing the old dropdown could not say', () => {
    // "critical and warning" was unsayable with a single-valued FilterSelect, and it is the first
    // question during triage.
    const crit = nodeAlert({ severity: 'critical' });
    const warn = nodeAlert({ severity: 'warning' });
    const info = nodeAlert({ severity: 'info' });
    const f = withFilters({ severity: 'critical,warning' });
    expect([matches(crit, f), matches(warn, f), matches(info, f)]).toEqual([true, true, false]);
  });

  it('splits acked from not-acked, and each excludes the other', () => {
    // The mirror-image assertion is the point: "Acked" showing un-acked rows was the History bug
    // this whole pass shipped a fix for, and it looked plausible from one direction only.
    const acked = nodeAlert({ acked: { source: 'pagerduty', by: 'oncall', at_unix_ms: 1 } });
    const plain = nodeAlert();
    expect(matches(acked, withFilters({ ack: 'acked' }))).toBe(true);
    expect(matches(plain, withFilters({ ack: 'acked' }))).toBe(false);
    expect(matches(plain, withFilters({ ack: 'unacked' }))).toBe(true);
    expect(matches(acked, withFilters({ ack: 'unacked' }))).toBe(false);
    // Ticking both is the same as ticking neither — and must not become "nothing matches".
    expect(matches(acked, withFilters({ ack: 'acked,unacked' }))).toBe(true);
    expect(matches(plain, withFilters({ ack: 'acked,unacked' }))).toBe(true);
    // And neither is applied at all when the filter is unset.
    expect(matches(acked, DEFAULTS)).toBe(true);
    expect(matches(plain, DEFAULTS)).toBe(true);
  });

  it('searches the resolved node name, the raw id and the metric', () => {
    const a = nodeAlert();
    expect(matches(a, withFilters({ q: 'rtr-0' }))).toBe(true);
    // The id is the handle in a deep link and in an API error, so pasting one must find its row.
    expect(matches(a, withFilters({ q: '0000000001' }))).toBe(true);
    expect(matches(a, withFilters({ q: 'icmp' }))).toBe(true);
    expect(matches(a, withFilters({ q: 'sw-04' }))).toBe(false);
  });

  it('searches case-insensitively and ignores surrounding space', () => {
    const a = nodeAlert();
    expect(matches(a, withFilters({ q: '  RTR-01 ' }))).toBe(true);
    // A term of nothing but spaces is not a filter.
    expect(matches(a, withFilters({ q: '   ' }))).toBe(true);
  });

  it('excludes with NOT and matches with a regex — neither was reachable before Inc.6', () => {
    const a = nodeAlert();
    expect(matches(a, withFilters({ q: '!icmp' }))).toBe(false);
    expect(matches(a, withFilters({ q: '!disk' }))).toBe(true);
    expect(matches(a, withFilters({ q: '~^rtr-\\d+$' }))).toBe(true);
    expect(matches(a, withFilters({ q: '~^sw-' }))).toBe(false);
  });

  it('matches a pool alert on its pool name, never through the resolver', () => {
    // `node` is `pool:tokyo` here, not a UUID — handing that to a name resolver is the mistake
    // `lib/alertSubject` exists to prevent.
    const spy = vi.fn(nameOf);
    expect(matches(poolAlert(), withFilters({ q: 'tokyo' }), spy)).toBe(true);
    expect(spy).not.toHaveBeenCalled();
    expect(matches(poolAlert(), withFilters({ q: 'osaka' }))).toBe(false);
  });

  it('applies every set filter together, not the first that matches', () => {
    const a = nodeAlert({ severity: 'warning' });
    expect(matches(a, withFilters({ severity: 'warning', q: 'icmp' }))).toBe(true);
    expect(matches(a, withFilters({ severity: 'critical', q: 'icmp' }))).toBe(false);
  });
});

describe('isAnyFiltered', () => {
  it('is false for the default view', () => {
    expect(isAnyFiltered(cols(), DEFAULTS)).toBe(false);
  });

  it('flips for every column, so a new filter cannot be forgotten here', () => {
    // Driven off the defaults rather than a hand-written list: a filter added without its clause
    // would otherwise make the screen say "there is nothing here" while a filter hides the rows.
    const sample: FilterState = {
      severity: 'critical',
      state: 'unreachable',
      ack: 'acked',
      q: 'rtr',
    };
    for (const c of cols()) {
      expect(isAnyFiltered(cols(), withFilters({ [c.key]: sample[c.key] }))).toBe(true);
    }
  });
});

describe('the URL codec', () => {
  it('reads an empty query as the default view', () => {
    expect(read('')).toEqual(DEFAULTS);
  });

  it('round-trips every field', () => {
    const f: FilterState = {
      severity: 'warning',
      state: 'maintenance',
      ack: 'unacked',
      q: 'rtr-01',
    };
    const params = new URLSearchParams();
    writeFilters(cols(), params, f);
    expect(read(params.toString())).toEqual(f);
  });

  it('keeps the single-token URLs that shipped before multi-value', () => {
    // The keys and the one-value spelling did not change, so a link taken from the old screen opens
    // the same view — a one-element set.
    expect(read('severity=critical').severity).toBe('critical');
    expect(read('ack=acked').ack).toBe('acked');
  });

  it('settles a set into the spec’s option order, whatever order the URL used', () => {
    // The joined value is compared for equality and used as an effect key, so a URL that varies with
    // click order would make two identical views compare unequal.
    expect(read('severity=warning,critical').severity).toBe(read('severity=critical,warning').severity);
  });

  it('leaves no query string at all for the default view', () => {
    const params = new URLSearchParams();
    writeFilters(cols(), params, DEFAULTS);
    expect(params.toString()).toBe('');
  });

  it('clears a key when its filter goes back to the default', () => {
    const params = new URLSearchParams('severity=critical&q=rtr&ack=acked');
    writeFilters(cols(), params, DEFAULTS);
    expect(params.toString()).toBe('');
  });

  it('drops an unknown token instead of rejecting the value', () => {
    // A stale bookmark or a hand-edited URL must not render a control whose value is not one of its
    // own options — the opposite of the API edge, where an unknown token is a 400.
    expect(read('severity=nuclear&state=melted&ack=maybe')).toEqual(DEFAULTS);
    // …and a mixed value keeps the half this build understands.
    expect(read('severity=nuclear,warning').severity).toBe('warning');
  });

  it('drops a search term that is only space', () => {
    const params = new URLSearchParams();
    writeFilters(cols(), params, withFilters({ q: '   ' }));
    // The term reaches the URL as typed, but it narrows nothing — `conditionIsActive` trims for
    // `contains`, which is what the predicate test above asserts.
    expect(matches(nodeAlert(), read(params.toString()))).toBe(true);
  });

  it('leaves query keys it does not own alone', () => {
    // The screen shares its URL with anything else that parks state there.
    const params = new URLSearchParams('tab=map&severity=info');
    writeFilters(cols(), params, DEFAULTS);
    expect(params.toString()).toBe('tab=map');
  });
});
