// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import { specColumns } from '../lib/columnFilter';
import type { DeliveryRow, NotificationChannel } from '../types/api';
import {
  appendPage,
  channelLabel,
  DEFAULT_ROUTE,
  deliveryFilters,
  durationText,
  nextCursor,
  PAGE_SIZE,
  queryFor,
  subjectText,
} from './deliveryLogQuery';

const t = ((key: string, opts?: Record<string, unknown>) =>
  opts?.kind ? `${key}:${String(opts.kind)}` : key) as unknown as TFunction;

function row(over: Partial<DeliveryRow> = {}): DeliveryRow {
  return {
    id: 1,
    at: '2026-10-04T00:00:00Z',
    channel_id: 'c1',
    channel_name: 'jsm-prod',
    channel_kind: 'jsm',
    event: 'fire',
    result: 'failed',
    side: 'remote',
    status: 401,
    attempts: 3,
    duration_ms: 1600,
    subject: 'n1',
    node_id: 'n1',
    subject_name: 'sw-01',
    severity: 'critical',
    error: 'unexpected status 401',
    response: '{"message":"Key format is not valid"}',
    attempt_log: [],
    ...over,
  };
}

const channels = [{ id: 'c1', name: 'jsm-prod' }] as NotificationChannel[];
const cols = specColumns(deliveryFilters(t, channels));

describe('delivery log query', () => {
  it('sends nothing for a filter nobody set', () => {
    const q = queryFor(cols, {}, null, 0);
    expect(q).toEqual({
      channel: undefined,
      event: undefined,
      result: undefined,
      side: undefined,
      since: undefined,
      before: undefined,
      before_id: undefined,
      limit: PAGE_SIZE,
    });
  });

  it('carries the channel, the default route and the sides the operator picked', () => {
    const q = queryFor(
      cols,
      { channel: `c1,${DEFAULT_ROUTE}`, side: 'remote,network', result: 'failed' },
      null,
      0,
    );
    expect(q.channel?.split(',').sort()).toEqual(['c1', DEFAULT_ROUTE].sort());
    expect(q.side?.split(',').sort()).toEqual(['network', 'remote']);
    expect(q.result).toBe('failed');
  });

  it('drops a token the options do not offer rather than sending it to be refused', () => {
    const q = queryFor(cols, { side: 'bogus', event: 'unknown' }, null, 0);
    expect(q.side).toBeUndefined();
    expect(q.event).toBeUndefined();
  });

  it('pages on both halves of the cursor', () => {
    const q = queryFor(cols, {}, { before: '2026-10-04T00:00:00Z', before_id: 9 }, 0);
    expect(q.before).toBe('2026-10-04T00:00:00Z');
    expect(q.before_id).toBe(9);
  });

  it('a short page is the end, a full one is not', () => {
    expect(nextCursor([row()])).toBeNull();
    const full = Array.from({ length: PAGE_SIZE }, (_, i) => row({ id: i + 1 }));
    expect(nextCursor(full)).toEqual({ before: full[PAGE_SIZE - 1].at, before_id: PAGE_SIZE });
  });

  it('appending a page drops rows already held', () => {
    expect(appendPage([row({ id: 1 })], [row({ id: 1 }), row({ id: 2 })]).map((r) => r.id)).toEqual(
      [1, 2],
    );
  });
});

describe('delivery log row text', () => {
  it('names the channel, the default route, or a deleted channel by its kind', () => {
    expect(channelLabel(t, row())).toBe('jsm-prod');
    expect(channelLabel(t, row({ channel_id: null, channel_name: null, channel_kind: null }))).toBe(
      'routing.log.defaultRoute',
    );
    expect(channelLabel(t, row({ channel_name: null }))).toBe(
      'routing.log.deletedChannelOfKind:Jira Service Management',
    );
  });

  it('a test send names no node', () => {
    expect(subjectText(t, row({ event: 'test', node_id: null, subject: 'test' }))).toBe(
      'routing.log.testSubject',
    );
    expect(subjectText(t, row({ node_id: null, subject: 'pool:edge', subject_name: null }))).toBe(
      'pool:edge',
    );
  });

  it('reads a duration the way a person writes it', () => {
    expect(durationText(85)).toBe('85 ms');
    expect(durationText(1600)).toBe('1.6 s');
    expect(durationText(31_500)).toBe('32 s');
  });
});
