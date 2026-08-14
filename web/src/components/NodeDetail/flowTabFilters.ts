// SPDX-License-Identifier: AGPL-3.0-only
// The Flow tab's drill-down filters (ADR-053 Inc.8), as pure functions.
//
// **These are not column filters and must never be placed under a table's headers.** They narrow
// all six flow queries at once — the trend, the four Top-N cards, the Sankey and the conversations
// table — so a row of controls sitting under the conversations headers would claim to filter that
// table and would in fact be re-asking every question on the tab. They go in a `FilterBar` at the
// top of the tab (決定 N), where what they govern is what is below them: everything.
//
// 🚨 **And the conversations table cannot have a filter row of its own either.** It holds
// `TOP_N = 10` rows that ClickHouse already ranked by bytes. A browser-side predicate over those
// would narrow ten rows out of thousands and then report "no conversations match" — an answer that
// is wrong in the direction that looks right. Narrowing conversations means asking the server for a
// different top-N, which is exactly what these filters do.
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md).

import {
  normalizeValues,
  specColumns,
  type ColumnFilterSpec,
  type FilterableColumn,
} from '../../lib/columnFilter';
import { PROTO_NAMES, portLabel } from '../../lib/flowLabels';
import type { TFunction } from 'i18next';

/**
 * How many values one drill-down may carry.
 *
 * ⚠️ **Mirrors `FLOW_FILTER_MAX` in `crates/yagra-core/src/api/flow.rs`, and nothing checks that it
 * still does.** The two must agree in one direction at least: a control that accepts more than the
 * endpoint takes turns a filter into a 400 the operator never sees coming, because the tab renders
 * a load error rather than a message about the control they just used.
 */
export const FLOW_FILTER_MAX = 8;

/** The rows the flow filters narrow — nothing, on this screen. Every one of them is server-side, so
 *  no spec here carries a row accessor and `buildPredicate` compiles all four to `null`. The type
 *  parameter exists only because `ColumnFilterSpec` is generic. */
type FlowRow = never;

/** Parse a decimal integer in `[0, max]`, or `null`. Shared by the three numeric drill-downs so
 *  "what counts as a port" cannot drift from "what counts as an ASN" by accident. */
function intToken(max: number): (token: string) => string | null {
  return (token) => {
    if (!/^\d+$/.test(token)) return null;
    const n = Number(token);
    return Number.isSafeInteger(n) && n >= 0 && n <= max ? String(n) : null;
  };
}

/**
 * An IP address token, normalized by the browser's own parser.
 *
 * ⚠️ **Deliberately permissive about *form*, strict about *shape*.** The backend parses these with
 * Rust's `IpAddr`, which this cannot call, so the goal here is to reject what is obviously not an
 * address (a hostname, a SQL fragment) without inventing a second, stricter definition that refuses
 * addresses the server would have accepted. A token that survives here and not there is a 400 the
 * operator can read; a token refused here that the server would have taken is a filter they cannot
 * express and cannot debug.
 */
export function ipToken(token: string): string | null {
  const v4 = /^(\d{1,3}\.){3}\d{1,3}$/;
  if (v4.test(token)) {
    return token.split('.').every((o) => Number(o) <= 255) ? token : null;
  }
  // IPv6: hex groups and colons only, with at most one `::`. Rust's parser is the real gate.
  if (/^[0-9a-fA-F:]+$/.test(token) && token.includes(':') && !/:::/.test(token)) {
    return token.toLowerCase();
  }
  return null;
}

/**
 * The four drill-downs.
 *
 * `proto` is the one with a closed vocabulary (`PROTO_NAMES`), so it is an `enum` and gains real
 * multi-select. The other three are typed sets (`values`, ADR-053 決定 P) — a port, an address or an
 * ASN has no list to pick from, and every other kind would describe a match the store does not run.
 */
export function flowFilters(t: TFunction) {
  return {
    proto: {
      kind: 'enum',
      options: Object.entries(PROTO_NAMES).map(([n, name]) => ({ value: n, label: name })),
      // No `readValue`: there is no row to read. ⚠️ This was written as `() => null` first, which a
      // test caught — a predicate built over it rejected every row, because `null` means "this row
      // has no value" and a selection then excludes it. `readValue` is optional for exactly this
      // case (`columnFilter.ts` records the trade).
      allLabel: t('flow.filter.allProtos'),
    },
    port: {
      kind: 'values',
      parse: intToken(65_535),
      format: (v) => portLabel(Number(v)),
      placeholder: t('flow.filter.port'),
      max: FLOW_FILTER_MAX,
    },
    peer: {
      kind: 'values',
      parse: ipToken,
      placeholder: t('flow.filter.peer'),
      max: FLOW_FILTER_MAX,
    },
    asn: {
      kind: 'values',
      parse: intToken(4_294_967_295),
      // `0` is the "unknown AS" bucket the Top-AS card shows, and it is selectable by clicking that
      // row — so it has to read as that bucket, not as "AS0".
      format: (v) => (v === '0' ? t('flow.as.unknown') : `AS${v}`),
      placeholder: t('flow.filter.asn'),
      max: FLOW_FILTER_MAX,
    },
  } satisfies Record<string, ColumnFilterSpec<FlowRow>>;
}

export function flowFilterColumns(t: TFunction): FilterableColumn<FlowRow>[] {
  return specColumns(flowFilters(t));
}

/** Plain-text names for the bar and the mobile sheet — a `FilterBar` has no header above a cell. */
export function flowFilterLabels(t: TFunction): Record<string, string> {
  return {
    proto: t('flow.filter.protoLabel'),
    port: t('flow.filter.portLabel'),
    peer: t('flow.filter.peerLabel'),
    asn: t('flow.filter.asnLabel'),
  };
}

/** The query parameters for one filter state. Empty sets are omitted entirely, so an unset filter
 *  never reaches the URL — the endpoint reads an absent parameter as "no filter" and an
 *  unparseable one as a 400. */
export function flowQueryFilters(state: Record<string, string>): {
  proto?: string;
  port?: string;
  peer?: string;
  asn?: string;
} {
  const set = (key: string) => (state[key] ?? '').trim() || undefined;
  return { proto: set('proto'), port: set('port'), peer: set('peer'), asn: set('asn') };
}

/**
 * Toggle one value in a drill-down set — what clicking a Top-N row, a Sankey node or a conversation
 * cell does.
 *
 * ⚠️ **This replaced a single-valued toggle and the difference is the point.** The old
 * `toggleFilterValue` *replaced* whatever was set, so clicking a second talker silently dropped the
 * first; now a second click adds it and a click on an active value removes it, which is what the
 * ✕ on the trigger already implied. Normalizing through the spec keeps a clicked value in the same
 * form a typed one takes, so the two cannot disagree about whether `010` and `10` are one value.
 */
export function toggleFlowValue(
  current: string,
  clicked: string,
  spec: ColumnFilterSpec<FlowRow>,
): string {
  const token = spec.kind === 'values' ? spec.parse(clicked.trim()) : clicked.trim();
  if (token === null || token === '') return current;
  const have = current ? current.split(',') : [];
  const next = have.includes(token) ? have.filter((v) => v !== token) : [...have, token];
  const joined = next.join(',');
  return spec.kind === 'values' ? normalizeValues(joined, spec.parse, spec.max) : joined;
}

/** Whether a port/peer/AS drill-down is set — those three cannot narrow the trend chart, because the
 *  5-minute rollup carries only `proto`. The tab says so rather than letting the chart look wrong. */
export function chartIgnoresFilters(state: Record<string, string>): boolean {
  return Boolean(state.port?.trim() || state.peer?.trim() || state.asn?.trim());
}
