// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Active-alerts triage filters (no DOM — Vitest node env).
//
// ADR-243 moved the screen onto the shared table with History's columns. Every property the
// earlier filter was tested for is still asserted — the interesting one being that a name is never
// resolved unless a node term was typed — plus the scope predicate the table gained.

import { describe, expect, it, vi } from 'vitest';
import { SEVERITIES, type NodeState } from '../types/api';
import { SEVERITY_ORDER } from '../lib/nodeState';
import {
  defaultFilters,
  isAnyFiltered,
  normalizeSets,
  readFilterParams,
  specColumns,
  writeFilterParams,
  type FilterState,
} from '../lib/columnFilter';
import { buildPredicate } from '../lib/filterPredicate';
import {
  activeAlertFilters,
  scopePredicate,
  type FilterableAlert,
  type GroupOf,
  type NameOf,
} from './activeAlertFilters';
import type { NoteKind } from '../lib/alertNotes';

/** A row as the tests build it: the notes are stated outright rather than derived. */
type TestAlert = FilterableAlert & { kinds?: NoteKind[] };

const NODE = 'aaaaaaaa-0000-0000-0000-000000000001';
const OTHER = 'aaaaaaaa-0000-0000-0000-000000000002';
const NAMES: Record<string, string> = { [NODE]: 'rtr-01' };
const nameOf: NameOf = (id) => NAMES[id] ?? id;

/** `t` is only ever used for labels here, so echoing the key is enough and keeps i18n out. */
const t = ((k: string) => k) as unknown as Parameters<typeof activeAlertFilters>[0];

const cols = (resolver: NameOf = nameOf) =>
  specColumns(
    activeAlertFilters<TestAlert>(
      t,
      resolver,
      (a) => a.kinds ?? [],
      SEVERITIES,
      SEVERITY_ORDER as readonly NodeState[],
    ),
  );

const DEFAULTS = defaultFilters(cols());
const withFilters = (over: FilterState): FilterState => ({ ...DEFAULTS, ...over });
const NOW = Date.UTC(2026, 10, 7);
const matches = (a: TestAlert, f: FilterState, resolver: NameOf = nameOf) =>
  buildPredicate(cols(resolver), f, NOW)(a);

const nodeAlert = (over: Partial<TestAlert> = {}): TestAlert => ({
  node: NODE,
  at_unix_ms: NOW - 60_000,
  subject_kind: 'node',
  severity: 'critical',
  state: 'unreachable',
  metric: 'icmp_rtt_ms',
  ...over,
});

const poolAlert = (over: Partial<TestAlert> = {}): TestAlert => ({
  node: 'pool:tokyo',
  at_unix_ms: NOW - 60_000,
  subject_kind: 'pool',
  subject_name: 'tokyo',
  severity: 'warning',
  state: 'unknown',
  metric: 'pool_coverage',
  ...over,
});

const read = (qs: string) => normalizeSets(cols(), readFilterParams(cols(), new URLSearchParams(qs)));

describe('the columns', () => {
  it('are History’s filter keys, so a link moves between the two screens by its path', () => {
    expect(cols().map((c) => c.key)).toEqual(['severity', 'node_q', 'metric', 'state', 'notes', 'range']);
  });
});

describe('the Notes and When columns', () => {
  it('keeps a row carrying any ticked kind of note, and drops a row with none', () => {
    const f = withFilters({ notes: 'flapping,held' });
    expect(matches(nodeAlert({ kinds: ['self', 'flapping'] }), f)).toBe(true);
    expect(matches(nodeAlert({ kinds: ['held'] }), f)).toBe(true);
    expect(matches(nodeAlert({ kinds: ['upstream'] }), f)).toBe(false);
    expect(matches(nodeAlert(), f)).toBe(false);
  });

  it('keeps an alert raised inside the window and drops an older one', () => {
    const f = withFilters({ range: '24h' });
    expect(matches(nodeAlert({ at_unix_ms: NOW - 3600_000 }), f)).toBe(true);
    expect(matches(nodeAlert({ at_unix_ms: NOW - 2 * 86400_000 }), f)).toBe(false);
  });

  it('opens on every age, so an old open alert is not hidden by default', () => {
    expect(matches(nodeAlert({ at_unix_ms: NOW - 400 * 86400_000 }), DEFAULTS)).toBe(true);
    expect(isAnyFiltered(cols(), DEFAULTS)).toBe(false);
  });
});

describe('the predicate', () => {
  it('shows everything when nothing is set', () => {
    expect(matches(nodeAlert(), DEFAULTS)).toBe(true);
    expect(matches(poolAlert(), DEFAULTS)).toBe(true);
  });

  it('never resolves a name unless a node term was typed', () => {
    // Load-bearing: `nameOf` is useEntityNames' lazy resolver and asking it about an id enqueues
    // that id for the next batch request. A predicate that asked unconditionally would resolve
    // every node in an outage in order to draw thirty rows.
    const spy = vi.fn(nameOf);
    matches(nodeAlert(), withFilters({ severity: 'critical', metric: 'icmp' }), spy);
    expect(spy).not.toHaveBeenCalled();
    matches(nodeAlert(), withFilters({ node_q: 'rtr' }), spy);
    expect(spy).toHaveBeenCalledWith(NODE);
  });

  it('filters by severity and by state independently, several values at once', () => {
    const a = nodeAlert({ severity: 'warning', state: 'warning' });
    expect(matches(a, withFilters({ severity: 'warning' }))).toBe(true);
    expect(matches(a, withFilters({ severity: 'critical' }))).toBe(false);
    expect(matches(a, withFilters({ severity: 'critical,warning' }))).toBe(true);
    expect(matches(a, withFilters({ state: 'warning' }))).toBe(true);
    expect(matches(a, withFilters({ state: 'unreachable' }))).toBe(false);
  });

  it('finds a node by its resolved name or its raw id, case-insensitively', () => {
    const a = nodeAlert();
    expect(matches(a, withFilters({ node_q: 'rtr-0' }))).toBe(true);
    // The id is the handle in a deep link and in an API error, so pasting one must find its row.
    expect(matches(a, withFilters({ node_q: '0000000001' }))).toBe(true);
    expect(matches(a, withFilters({ node_q: '  RTR-01 ' }))).toBe(true);
    expect(matches(a, withFilters({ node_q: 'sw-04' }))).toBe(false);
    // The node column does not search the metric any more — that is the What column's job.
    expect(matches(a, withFilters({ node_q: 'icmp' }))).toBe(false);
  });

  it('finds the metric in the What column, and never a liveness alert that stores none', () => {
    expect(matches(nodeAlert(), withFilters({ metric: 'icmp' }))).toBe(true);
    expect(matches(nodeAlert(), withFilters({ metric: 'disk' }))).toBe(false);
    expect(matches(nodeAlert({ metric: null }), withFilters({ metric: 'icmp' }))).toBe(false);
  });

  it('excludes with NOT and matches with a regex', () => {
    const a = nodeAlert();
    expect(matches(a, withFilters({ metric: '!icmp' }))).toBe(false);
    expect(matches(a, withFilters({ metric: '!disk' }))).toBe(true);
    expect(matches(a, withFilters({ node_q: '~^rtr-\\d+$' }))).toBe(true);
    expect(matches(a, withFilters({ node_q: '~^sw-' }))).toBe(false);
  });

  it('matches a pool alert on its pool name, never through the resolver', () => {
    // `node` is `pool:tokyo` here, not a UUID — handing that to a name resolver is the mistake
    // `lib/alertSubject` exists to prevent.
    const spy = vi.fn(nameOf);
    expect(matches(poolAlert(), withFilters({ node_q: 'tokyo' }), spy)).toBe(true);
    expect(spy).not.toHaveBeenCalled();
    expect(matches(poolAlert(), withFilters({ node_q: 'osaka' }))).toBe(false);
  });

  it('applies every set filter together, not the first that matches', () => {
    const a = nodeAlert({ severity: 'warning' });
    expect(matches(a, withFilters({ severity: 'warning', metric: 'icmp' }))).toBe(true);
    expect(matches(a, withFilters({ severity: 'critical', metric: 'icmp' }))).toBe(false);
  });
});

describe('isAnyFiltered', () => {
  it('is false for the default view and flips for every column', () => {
    expect(isAnyFiltered(cols(), DEFAULTS)).toBe(false);
    const sample: FilterState = {
      severity: 'critical',
      node_q: 'rtr',
      metric: 'icmp',
      state: 'unreachable',
    };
    for (const c of cols()) {
      expect(isAnyFiltered(cols(), withFilters({ [c.key]: sample[c.key] }))).toBe(true);
    }
  });
});

describe('the URL codec', () => {
  it('reads an empty query as the default view, and round-trips every field', () => {
    expect(read('')).toEqual(DEFAULTS);
    const f: FilterState = {
      severity: 'warning',
      node_q: 'rtr-01',
      metric: 'icmp',
      state: 'maintenance',
      notes: 'flapping',
      range: '7d',
    };
    const params = new URLSearchParams();
    writeFilterParams(cols(), params, f);
    expect(read(params.toString())).toEqual(f);
  });

  it('keeps the severity and state links that shipped before ADR-243', () => {
    expect(read('severity=critical').severity).toBe('critical');
    expect(read('state=unreachable').state).toBe('unreachable');
  });

  it('ignores the retired `q` key rather than reading it as a filter', () => {
    // `q` searched node and metric at once; ADR-243 split it into History's two columns. An old
    // link opens the unfiltered view rather than a view narrowed by something no control shows.
    expect(read('q=rtr')).toEqual(DEFAULTS);
  });

  it('leaves no query string for the default view, and leaves keys it does not own alone', () => {
    const params = new URLSearchParams('severity=critical&node_q=rtr&node_id=x');
    writeFilterParams(cols(), params, DEFAULTS);
    expect(params.toString()).toBe('node_id=x');
  });
});

describe('scopePredicate', () => {
  const GROUP = 'g-site';
  const groups: Record<string, string | null> = { [NODE]: GROUP, [OTHER]: 'g-elsewhere' };
  const groupOf: GroupOf = (id) => (id in groups ? groups[id] : undefined);
  const subtree = new Set([GROUP, 'g-site-floor-2']);

  it('is null when nothing is scoped, so the caller skips the pass', () => {
    expect(scopePredicate({ nodeId: '', groupId: '' }, subtree, groupOf)).toBeNull();
  });

  it('keeps one node’s alerts, and never a pool’s', () => {
    const p = scopePredicate({ nodeId: NODE, groupId: '' }, subtree, groupOf)!;
    expect(p(nodeAlert())).toBe(true);
    expect(p(nodeAlert({ node: OTHER }))).toBe(false);
    expect(p(poolAlert())).toBe(false);
  });

  it('keeps the nodes filed in the folder or beneath it', () => {
    const p = scopePredicate({ nodeId: '', groupId: GROUP }, subtree, groupOf)!;
    expect(p(nodeAlert())).toBe(true);
    expect(p(nodeAlert({ node: OTHER }))).toBe(false);
    expect(p(poolAlert())).toBe(false);
  });

  it('leaves out a node whose folder has not resolved yet, and an ungrouped one', () => {
    const p = scopePredicate({ nodeId: '', groupId: GROUP }, subtree, (id) =>
      id === NODE ? undefined : null,
    )!;
    expect(p(nodeAlert())).toBe(false);
    expect(p(nodeAlert({ node: OTHER }))).toBe(false);
  });
});
