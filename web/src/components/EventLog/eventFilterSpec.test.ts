// SPDX-License-Identifier: AGPL-3.0-only
// The Events filter row: what the specs promise, and what the state becomes on the wire.
//
// This is the screen where a filter is a *query*, not a predicate, so these are the tests that
// stand between the filter row and two stores answering different questions. There is no component
// test to back them up (`environment: 'node'` — testing.md), which is why the judgement lives in
// the module they cover.

import { describe, expect, it } from 'vitest';
import {
  EVENT_FILTER_KEYS,
  eventEmptyKind,
  eventFilterColumns,
  eventFilterKey,
  eventFilterQuery,
  eventFilters,
} from './eventFilterSpec';
import { DEFAULT_EVENT_RANGE, EVENT_RANGES } from './eventRange';
import {
  defaultFilters,
  isAnyFiltered,
  reservedKeyCollisions,
  type FilterState,
} from '../../lib/columnFilter';
import { EVENT_ACTIONS, EVENT_KINDS } from '../../types/api';

/** A translator stand-in that returns the key, so a missing label is visible as its key. */
const t = ((k: string) => k) as unknown as Parameters<typeof eventFilters>[0];

const COLUMNS = eventFilterColumns(t);
const DEFAULTS = defaultFilters(COLUMNS);
const NOW = Date.parse('2026-08-13T12:00:00Z');

describe('the spec', () => {
  it('offers a filter on every column that has one, in column order', () => {
    expect(COLUMNS.map((c) => c.key)).toEqual([...EVENT_FILTER_KEYS]);
  });

  it('drops the source column inside a single node tab', () => {
    // Every row there is that node, so a source filter is a control that can only narrow to
    // nothing or to everything.
    expect(eventFilterColumns(t, { showSource: false }).map((c) => c.key)).toEqual([
      'kind',
      'message',
      'action',
      'at',
    ]);
  });

  it('takes its option sets from the shared enums rather than a local list', () => {
    // A backend variant added without a UI entry is the drift `extensibility.md` §4 is about, and
    // for these two the failure is invisible: the operator sees a filter that silently cannot name
    // a value their events actually carry.
    const specs = eventFilters(t);
    const kind = specs.kind;
    const action = specs.action;
    expect(kind.kind === 'enum' && kind.options.map((o) => o.value)).toEqual([...EVENT_KINDS]);
    expect(action.kind === 'enum' && action.options.map((o) => o.value)).toEqual([...EVENT_ACTIONS]);
  });

  it('keeps the bounded range default', () => {
    // Moved here from `eventRange.test.ts`'s sibling assertion rather than deleted: the default is
    // a **performance contract** (a case-insensitive term is ~10× unbounded), not a preference, and
    // the filter row is now what chooses it.
    expect(DEFAULTS.at).toBe(DEFAULT_EVENT_RANGE);
    expect(DEFAULTS.at).not.toBe('all');
    expect(isAnyFiltered(COLUMNS, DEFAULTS)).toBe(false);
  });

  it('uses column keys that do not collide with the page own query params', () => {
    expect(reservedKeyCollisions(COLUMNS)).toEqual([]);
  });

  it('offers a regex mode on the message and not on the source', () => {
    // There is no `src_regex` parameter, because a source match spans the node *name*, which the
    // log store has never heard of. Offering the toggle would promise something one backend cannot
    // do — the Rust side pins the same fact from its end.
    const specs = eventFilters(t);
    expect(specs.message.kind === 'text' && specs.message.modes).toEqual(['contains', 'regex']);
    expect(specs.source.kind === 'text' && specs.source.modes).toEqual(['contains']);
  });

  it('only warns about whole-word matching on a deployment that does it', () => {
    const token = eventFilters(t, { semantics: 'token' });
    const substring = eventFilters(t, { semantics: 'substring' });
    const unknown = eventFilters(t, {});
    expect(token.message.kind === 'text' && token.message.containsSemantics).toBe('token');
    // Substring needs no warning; an N-1 core that reports nothing must not have one invented for
    // it, because the guess would be wrong exactly half the time.
    expect(substring.message.kind === 'text' && substring.message.containsSemantics).toBeUndefined();
    expect(unknown.message.kind === 'text' && unknown.message.containsSemantics).toBeUndefined();
  });
});

describe('eventFilterQuery', () => {
  it('sends nothing but the default window when nothing is set', () => {
    const q = eventFilterQuery(DEFAULTS, NOW);
    expect(q).toEqual({ start: '2026-08-12T12:00:00.000Z' });
  });

  it('joins a multi-select with commas', () => {
    // Commas rather than repeated parameters: `buildUrl` uses `params.set`, and `api.test.ts` pins
    // the resulting URL verbatim. The API edge splits and validates each token.
    const q = eventFilterQuery({ ...DEFAULTS, kind: 'syslog,trap', action: 'fired' }, NOW);
    expect(q.kind).toBe('syslog,trap');
    expect(q.action).toBe('fired');
  });

  it('decomposes a text condition into its term, mode and negation', () => {
    expect(eventFilterQuery({ ...DEFAULTS, message: 'link down' }, NOW)).toMatchObject({
      msg: 'link down',
    });
    const negatedRegex = eventFilterQuery({ ...DEFAULTS, message: '!~^LINK' }, NOW);
    expect(negatedRegex).toMatchObject({ msg: '^LINK', msg_regex: true, msg_not: true });
    // The flags are omitted rather than sent false, so the URL of a plain search stays short and
    // the API's `unwrap_or(false)` is what supplies the default.
    expect(Object.keys(eventFilterQuery({ ...DEFAULTS, message: 'x' }, NOW))).toEqual([
      'msg',
      'start',
    ]);
  });

  it('sends a source condition with no regex flag, ever', () => {
    const q = eventFilterQuery({ ...DEFAULTS, source: '!~rtr' }, NOW);
    // `~` decodes as regex mode, but this column offers no such mode and the wire has no parameter
    // for it — so the pattern travels as a plain term rather than being silently dropped.
    expect(q.src).toBe('rtr');
    expect(q.src_not).toBe(true);
    expect(Object.keys(q)).not.toContain('src_regex');
  });

  it('resolves a preset to a lower bound and all-time to neither', () => {
    expect(eventFilterQuery({ ...DEFAULTS, at: '7d' }, NOW).start).toBe('2026-08-06T12:00:00.000Z');
    const all = eventFilterQuery({ ...DEFAULTS, at: 'all' }, NOW);
    expect(all.start).toBeUndefined();
    expect(all.end).toBeUndefined();
  });

  it('carries a custom window inside the one value', () => {
    // The instants ride in the column's own value rather than in sibling `from`/`to` keys: the
    // filter state is flat by construction, and those two names are reserved for the pages.
    const q = eventFilterQuery(
      { ...DEFAULTS, at: 'custom:2026-08-01T00:00|2026-08-02T00:00' },
      NOW,
    );
    expect(q.start).toBe(new Date('2026-08-01T00:00').toISOString());
    expect(q.end).toBe(new Date('2026-08-02T00:00').toISOString());
  });

  it('treats an empty custom window as unbounded rather than as an error', () => {
    const q = eventFilterQuery({ ...DEFAULTS, at: 'custom' }, NOW);
    expect(q.start).toBeUndefined();
    expect(q.end).toBeUndefined();
  });

  it('falls back to the default window for a preset this build does not have', () => {
    // A bookmark from a build with different presets must land on the default view, never send a
    // value the API would reject or, worse, one it would ignore.
    expect(eventFilterQuery({ ...DEFAULTS, at: 'last-fortnight' }, NOW).start).toBe(
      '2026-08-12T12:00:00.000Z',
    );
  });

  it('resolves the window from the passed clock, not the wall clock', () => {
    // The reason `boundsFor` takes `nowMs`: a lower bound recomputed per request creeps forward
    // between "load older" pages and drops rows the keyset cursor was walking towards.
    const later = eventFilterQuery(DEFAULTS, NOW + 3_600_000).start;
    expect(later).toBe('2026-08-12T13:00:00.000Z');
  });
});

describe('eventFilterKey', () => {
  it('is stable across calls and changes with any dimension', () => {
    const base = eventFilterQuery(DEFAULTS, NOW);
    expect(eventFilterKey(base)).toBe(eventFilterKey(eventFilterQuery(DEFAULTS, NOW)));
    const dims: FilterState[] = [
      { ...DEFAULTS, kind: 'trap' },
      { ...DEFAULTS, action: 'fired' },
      { ...DEFAULTS, message: 'x' },
      { ...DEFAULTS, message: '!x' },
      { ...DEFAULTS, message: '~x' },
      { ...DEFAULTS, source: 'y' },
      { ...DEFAULTS, source: '!y' },
      { ...DEFAULTS, at: '7d' },
    ];
    const keys = dims.map((s) => eventFilterKey(eventFilterQuery(s, NOW)));
    expect(new Set(keys).size).toBe(keys.length);
    expect(keys).not.toContain(eventFilterKey(base));
  });
});

describe('eventEmptyKind', () => {
  it('names the window when nothing is filtered', () => {
    // The default *narrows*, so "there are no events" would be a lie — there are none in the last
    // 24 hours, which is a different sentence and a different next action.
    expect(eventEmptyKind(DEFAULTS, 'token', false)).toBe('unfiltered');
  });

  it('names the whole-word rule when a plain term found nothing on a log store', () => {
    // `%%01POLICY/6/POLICYPERMIT` tokenizes to `01policy` and `policypermit`, so `POLICY` matches
    // nothing while the operator is looking at the word. This is the case the generic message
    // cannot explain, and the reason the deployment reports its search semantics at all.
    expect(eventEmptyKind({ ...DEFAULTS, message: 'POLICY' }, 'token', true)).toBe('tokenMiss');
    expect(eventEmptyKind({ ...DEFAULTS, source: 'rtr' }, 'token', true)).toBe('tokenMiss');
  });

  it('does not blame tokenization for a case it cannot explain', () => {
    // A regex reaches inside words on either store; a negated term returning nothing means
    // *everything* matched. Neither is a tokenization miss, and saying so would send the operator
    // to the wrong fix.
    expect(eventEmptyKind({ ...DEFAULTS, message: '~POLICY' }, 'token', true)).toBe('filtered');
    expect(eventEmptyKind({ ...DEFAULTS, message: '!POLICY' }, 'token', true)).toBe('filtered');
    // And on a substring deployment there is nothing to explain in the first place.
    expect(eventEmptyKind({ ...DEFAULTS, message: 'POLICY' }, 'substring', true)).toBe('filtered');
    expect(eventEmptyKind({ ...DEFAULTS, message: 'POLICY' }, undefined, true)).toBe('filtered');
    // A non-text filter finding nothing is just an empty result.
    expect(eventEmptyKind({ ...DEFAULTS, kind: 'trap' }, 'token', true)).toBe('filtered');
  });
});

describe('the range presets', () => {
  it('offers every preset the range module defines', () => {
    const at = eventFilters(t).at;
    expect(at.kind === 'range' && at.presets.map((p) => p.value)).toEqual([...EVENT_RANGES]);
  });
});
