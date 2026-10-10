// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Notification delivery — the delivery log's query logic, as pure functions (ADR-195).
//
// Here rather than in the page because Vitest never executes a `.tsx` (testing.md). Everything
// that decides *what is asked for* and *how a row reads* lives here; `DeliveryLog.tsx` is layout.
//
// Server-side filters, like the audit log: the log grows with every alert the fleet raises, so it
// is a list that "gets longer when the customer adds nodes" (ui-conventions) and the predicate runs
// in SQL.

import type { TFunction } from 'i18next';
import {
  normalizeSets,
  rangeSecondsIn,
  type ColumnFilterSpec,
  type FilterState,
  type FilterableColumn,
} from '../lib/columnFilter';
import { sinceIso, unset } from '../lib/filterQuery';
import { rangePresets, type RangeToken } from '../lib/filterPresets';
import { channelKindLabel } from '../lib/channelKinds';
import {
  DELIVERY_EVENTS,
  DELIVERY_RESULTS,
  DELIVERY_SIDES,
  type DeliveryQuery,
  type DeliveryRow,
  type DeliverySide,
  type NotificationChannel,
} from '../types/api';
import { appendPage, nextCursorFrom } from '../lib/keysetPage';

/** URL-key prefix for this table. The route already has `channels.` and `rules.`, and all three
 *  would otherwise share a `status`-style key; `filterSpecRegistry.test.ts` checks they are
 *  disjoint (ADR-153 decision 3). */
export const DELIVERY_FILTER_PREFIX = 'log.';

/** Rows per request: the backend's default, under its 500 ceiling, so a short page means the end. */
export const PAGE_SIZE = 100;

/** The token the `channel` filter uses for the environment default route, which has no channel id.
 *  The backend accepts the same word. */
export const DEFAULT_ROUTE = 'default';

/** The time windows the screen offers. */
export const DELIVERY_RANGES = ['24h', '7d', '30d', 'all'] as const satisfies readonly RangeToken[];

export type DeliveryColumns = readonly FilterableColumn<DeliveryRow>[];

/** The filter row, keyed by `Column.key`, declared in the table's column order. No column carries a
 *  row accessor: every filter is applied by the server. */
export function deliveryFilters(
  t: TFunction,
  channels: readonly NotificationChannel[],
): Record<string, ColumnFilterSpec<DeliveryRow>> {
  return {
    range: {
      kind: 'range',
      presets: rangePresets(DELIVERY_RANGES, t),
      // `all`, for the audit log's reason: the table is indexed on the column the cursor pages
      // on, so the newest page costs the same however long the log is.
      defaultPreset: 'all',
    },
    channel: {
      kind: 'enum',
      options: [
        ...channels.map((c) => ({ value: c.id, label: c.name })),
        { value: DEFAULT_ROUTE, label: t('routing.log.defaultRoute') },
      ],
      allLabel: t('routing.log.filter.allChannels'),
    },
    event: {
      kind: 'enum',
      options: DELIVERY_EVENTS.filter((e) => e !== 'unknown').map((e) => ({
        value: e,
        label: t(`routing.log.event.${e}`),
      })),
      allLabel: t('routing.log.filter.allEvents'),
    },
    result: {
      kind: 'enum',
      options: DELIVERY_RESULTS.filter((r) => r !== 'unknown').map((r) => ({
        value: r,
        label: t(`routing.log.result.${r}`),
      })),
      allLabel: t('routing.log.filter.allResults'),
    },
    side: {
      kind: 'enum',
      options: DELIVERY_SIDES.filter((s) => s !== 'unknown').map((s) => ({
        value: s,
        label: t(`routing.log.side.${s}`),
      })),
      allLabel: t('routing.log.filter.allSides'),
    },
  };
}

/** The keyset cursor for the page after `rows`, or `null` when there is no next page. */
export interface DeliveryCursor {
  before: string;
  before_id: number;
}

export function nextCursor(rows: readonly DeliveryRow[]): DeliveryCursor | null {
  return nextCursorFrom(rows, PAGE_SIZE, (last) => ({ before: last.at, before_id: last.id }));
}

/**
 * The request for one page. Every unset filter is `undefined`, never `''` — an empty string would
 * reach the backend as a value and be refused. `normalizeSets` drops a token a hand-typed URL
 * carried that the options do not offer.
 */
export function queryFor(
  columns: DeliveryColumns,
  s: FilterState,
  cursor: DeliveryCursor | null,
  nowMs: number,
): DeliveryQuery {
  const f = normalizeSets(columns, s);
  return {
    channel: unset(f.channel),
    event: unset(f.event),
    result: unset(f.result),
    side: unset(f.side),
    since: sinceIso(rangeSecondsIn(columns, f), nowMs),
    before: cursor?.before,
    before_id: cursor?.before_id,
    limit: PAGE_SIZE,
  };
}

/** Append a page, dropping rows already held (a duplicate React key misrenders silently). */
export { appendPage };

/** What the channel column says: the channel's name, the default route, or - for a channel deleted
 *  since - its kind, so the row is still readable. */
export function channelLabel(t: TFunction, r: DeliveryRow): string {
  if (r.channel_id == null) return t('routing.log.defaultRoute');
  if (r.channel_name) return r.channel_name;
  return r.channel_kind
    ? t('routing.log.deletedChannelOfKind', { kind: channelKindLabel(r.channel_kind) })
    : t('routing.log.deletedChannel');
}

/** Milliseconds as a person reads them: `85 ms`, `1.6 s`, `31.5 s`. */
export function durationText(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  const s = ms / 1000;
  return `${s < 10 ? s.toFixed(1) : Math.round(s)} s`;
}

/** The one sentence that answers "whose problem is this", for a failed row's side. */
export function sideExplanationKey(side: DeliverySide): string {
  return `routing.log.sideExplain.${side}`;
}

/** The subject column for a row that is not about a node (a node is drawn by `EntityName`): a test
 *  send, a poller pool, a Meraki organization. Names what was resolved at delivery time, else the
 *  stored identifier. */
export function subjectText(t: TFunction, r: DeliveryRow): string {
  if (r.event === 'test' || r.event === 'test_close') return t('routing.log.testSubject');
  return r.subject_name ?? r.subject;
}
