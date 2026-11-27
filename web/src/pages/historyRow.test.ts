// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { AlertHistoryRow, DeliveryRow } from '../types/api';
import {
  deliveriesOf,
  deliveryEventsOf,
  deliveryQueryOf,
  GROUPED_MEMBERS_LIMIT,
  groupedUnderOf,
  hasDetail,
  mayHoldGrouped,
  memberNodes,
  membersCut,
  momentAmong,
  momentOf,
  nextTransitionOf,
  phaseOf,
} from './historyRow';

const LIVENESS = '__liveness__';

const row = (over: Partial<AlertHistoryRow> = {}): AlertHistoryRow => ({
  id: 'h1',
  node: 'sw-01',
  subject_kind: 'node',
  check: 'c-sw',
  severity: 'critical',
  state: 'unreachable',
  at_unix_ms: Date.parse('2026-11-26T10:00:00Z'),
  resolved: false,
  rolled_up: false,
  metric: LIVENESS,
  recorded_at: '2026-11-26T10:00:00Z',
  ...over,
});

const delivery = (at: string, over: Partial<DeliveryRow> = {}): DeliveryRow =>
  ({
    id: 1,
    at,
    event: 'fire',
    result: 'delivered',
    subject: 'sw-01',
    route_rules: [],
    attempts: 1,
    duration_ms: 10,
    attempt_log: [],
    ...over,
  }) as DeliveryRow;

describe('a History row (ADR-270)', () => {
  it('reads a fire, a clear and a roll-up apart', () => {
    expect(phaseOf(row())).toBe('fired');
    expect(phaseOf(row({ resolved: true }))).toBe('cleared');
    expect(phaseOf(row({ rolled_up: true }))).toBe('rolledUp');
  });

  it('names the outage an up/down row was grouped into, and not its own node', () => {
    expect(groupedUnderOf(row({ node: 'ap-01', root_cause: 'sw-01' }))).toBe('sw-01');
    // A down node's other alerts carry their own node (ADR-087): no grouping.
    expect(groupedUnderOf(row({ root_cause: 'sw-01' }))).toBeNull();
    expect(groupedUnderOf(row({ node: 'ap-01', root_cause: 'sw-01', metric: 'snmp_up' }))).toBeNull();
  });

  it('asks for members only on an up/down fire grouped under nobody', () => {
    expect(mayHoldGrouped(row())).toBe(true);
    expect(mayHoldGrouped(row({ resolved: true }))).toBe(false);
    expect(mayHoldGrouped(row({ rolled_up: true }))).toBe(false);
    expect(mayHoldGrouped(row({ node: 'ap-01', root_cause: 'sw-01' }))).toBe(false);
    expect(mayHoldGrouped(row({ metric: 'cpu_util_pct' }))).toBe(false);
  });

  it('dates a roll-up by when it was written, every other row by its own time', () => {
    const fire = row();
    expect(momentOf(fire)).toBe(Date.parse('2026-11-26T10:00:00Z'));
    // The roll-up carries its fire's time; its own moment is later.
    const rolled = row({ id: 'h2', rolled_up: true, recorded_at: '2026-11-26T10:20:00Z' });
    expect(momentOf(rolled)).toBe(Date.parse('2026-11-26T10:20:00Z'));
    expect(nextTransitionOf(fire, [rolled, fire])).toBe(Date.parse('2026-11-26T10:20:00Z'));
    expect(nextTransitionOf(rolled, [rolled, fire])).toBeNull();
  });

  it('opens only a row that has something to show', () => {
    // A threshold fire on a node: only what it sent, which a Viewer may not read.
    const threshold = row({ metric: 'cpu_util_pct' });
    expect(hasDetail(threshold, false)).toBe(false);
    expect(hasDetail(threshold, true)).toBe(true);
    // An up/down fire may hold grouped nodes; a grouped member names its outage.
    expect(hasDetail(row(), false)).toBe(true);
    expect(hasDetail(row({ node: 'ap-01', root_cause: 'sw-01', resolved: true }), false)).toBe(true);
    // A pool alert has no node to ask about.
    expect(hasDetail(row({ node: null, subject_kind: 'pool', metric: 'pool' }), true)).toBe(false);
  });

  it('says the member list is cut when the server stopped at its limit', () => {
    expect(membersCut(new Array(GROUPED_MEMBERS_LIMIT - 1))).toBe(false);
    expect(membersCut(new Array(GROUPED_MEMBERS_LIMIT))).toBe(true);
  });

  it('lists each member node once, in the order given', () => {
    expect(memberNodes([row({ node: 'a' }), row({ node: 'b' }), row({ node: 'a' }), row({ node: null })])).toEqual([
      'a',
      'b',
    ]);
  });

  it('asks the delivery log for this alert, the kinds this transition sends, from just before it', () => {
    expect(deliveryEventsOf(row())).toEqual(['fire', 'update']);
    expect(deliveryEventsOf(row({ resolved: true }))).toEqual(['resolve']);
    expect(deliveryEventsOf(row({ rolled_up: true }))).toEqual(['suppress']);
    expect(deliveryQueryOf(row(), [row()])).toEqual({
      subject: 'sw-01',
      check_id: 'c-sw',
      event: 'fire,update',
      since: '2026-11-26T09:59:00.000Z',
      limit: 100,
    });
    // Up to the same check's next transition on screen, so a later outage cannot use up the limit.
    const clear = row({ id: 'h2', resolved: true, at_unix_ms: Date.parse('2026-11-26T10:30:00Z') });
    expect(deliveryQueryOf(row(), [clear, row()])?.until).toBe('2026-11-26T10:30:00.000Z');
    // A roll-up asks from its own moment, not from the fire it closes.
    const rolled = row({ id: 'h3', rolled_up: true, recorded_at: '2026-11-26T10:20:00Z' });
    expect(deliveryQueryOf(rolled, [rolled])?.since).toBe('2026-11-26T10:19:00.000Z');
    expect(deliveryQueryOf(row({ node: null, subject_kind: 'pool' }), [])).toBeNull();
  });

  it('reads the deliveries of a fire that pages again after its outage let it go as its own', () => {
    // ap-02 paged at 10:00, was grouped into sw-01's outage at 10:20, and paged again at 10:40
    // when sw-01 came back. The engine sends that last fire with the first one's time.
    const first = row({ node: 'ap-02', check: 'c-ap' });
    const rolled = row({ id: 'h2', node: 'ap-02', check: 'c-ap', rolled_up: true, recorded_at: '2026-11-26T10:20:00Z' });
    const again = row({ id: 'h3', node: 'ap-02', check: 'c-ap', recorded_at: '2026-11-26T10:40:00Z' });
    const rows = [again, rolled, first];
    expect(momentAmong(again, rows)).toBe(Date.parse('2026-11-26T10:40:00Z'));
    expect(momentAmong(first, rows)).toBe(Date.parse('2026-11-26T10:00:00Z'));
    expect(deliveryQueryOf(again, rows)?.since).toBe('2026-11-26T10:39:00.000Z');
    expect(deliveryQueryOf(again, rows)?.until).toBeUndefined();
    expect(deliveryQueryOf(first, rows)?.until).toBe('2026-11-26T10:20:00.000Z');
    expect(nextTransitionOf(rolled, rows)).toBe(Date.parse('2026-11-26T10:40:00Z'));
    const sent = [
      delivery('2026-11-26T10:41:00Z', { id: 3 }),
      delivery('2026-11-26T10:20:01Z', { id: 2, event: 'suppress' }),
      delivery('2026-11-26T10:00:01Z', { id: 1 }),
    ];
    // The first fire's own page, and not the roll-up's close or the second page.
    expect(deliveriesOf(first, rows, sent).map((d) => d.id)).toEqual([1]);
  });

  it('keeps the deliveries sent before the same check next changed, oldest first', () => {
    const fire = row();
    const clear = row({ id: 'h2', resolved: true, at_unix_ms: Date.parse('2026-11-26T10:30:00Z') });
    const other = row({ id: 'h3', check: 'c-ap', at_unix_ms: Date.parse('2026-11-26T10:05:00Z') });
    const sent = [
      delivery('2026-11-26T10:40:00Z', { id: 3 }), // after the clear: the next outage's
      delivery('2026-11-26T10:10:00Z', { id: 2, event: 'update' }),
      delivery('2026-11-26T10:00:01Z', { id: 1 }),
    ];
    expect(deliveriesOf(fire, [clear, other, fire], sent).map((d) => d.id)).toEqual([1, 2]);
    // With no later transition on screen, every one is kept.
    expect(deliveriesOf(fire, [fire], sent).map((d) => d.id)).toEqual([1, 2, 3]);
  });
});
