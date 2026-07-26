// SPDX-License-Identifier: AGPL-3.0-only
/** Pure helpers for rendering a DNS resolution chain (ADR-033).
 *
 *  The layout logic lives here rather than in the component so it can be unit-tested without a
 *  DOM — the same split `FlowSankey`/`TopologyMap` use for their geometry. */

import type { DnsChain, DnsFailure, DnsRecord } from '../../types/api';

/** One rendered step of the chain: the name asked, how it was answered, and what it led to. */
export interface DnsChainRow {
  /** The name queried at this hop (already normalized by the poller). */
  name: string;
  /** Record type of the answers, e.g. `CNAME` / `A`. `null` when the hop had no answers. */
  edge: string | null;
  /** The answer values in canonical order — several when an RRset resolved to many addresses. */
  values: string[];
  /** TTL of the first answer, for the hover title. `null` when there were no answers. */
  ttl: number | null;
  /** Whether this is the last hop (the terminal record set). */
  terminal: boolean;
}

/** The record type label shown on the edge between two hops. */
export function recordTypeLabel(record: DnsRecord): string {
  switch (record.kind) {
    case 'cname':
      return 'CNAME';
    case 'a':
      return 'A';
    case 'aaaa':
      return 'AAAA';
  }
}

/** The rendered value of one answer (a CNAME target or an address). */
export function recordValue(record: DnsRecord): string {
  return record.kind === 'cname' ? record.target : record.addr;
}

/** Flatten a chain into the rows the strip renders, in walk order.
 *
 *  Hop order is significant and is never re-sorted; the answers inside a hop already arrive in the
 *  poller's canonical (sorted, deduped) order, so the UI never shows a random ordering. */
export function chainToRows(chain: DnsChain): DnsChainRow[] {
  return chain.hops.map((hop, i) => ({
    name: hop.name,
    edge: hop.answers.length > 0 ? recordTypeLabel(hop.answers[0].record) : null,
    values: hop.answers.map((a) => recordValue(a.record)),
    ttl: hop.answers.length > 0 ? hop.answers[0].ttl : null,
    terminal: i === chain.hops.length - 1,
  }));
}

/** The i18n key for a failure, plus the interpolation values it needs.
 *
 *  Returns `null` when the chain resolved, so callers can branch on truthiness. */
export function failureLabel(
  failure: DnsFailure | null,
): { key: string; values: Record<string, string | number> } | null {
  if (!failure) return null;
  switch (failure.kind) {
    case 'other_rcode':
      return { key: 'overview.dnsFailure.other_rcode', values: { rcode: failure.rcode } };
    case 'loop_detected':
      return { key: 'overview.dnsFailure.loop_detected', values: { at: failure.at } };
    case 'depth_exceeded':
      return {
        key: 'overview.dnsFailure.depth_exceeded',
        values: { maxDepth: failure.max_depth },
      };
    default:
      return { key: `overview.dnsFailure.${failure.kind}`, values: {} };
  }
}

/** A one-line summary of a chain, for the history table.
 *
 *  Reads like the `dig` answer it represents: `horryworks.net → horry.net → 10.1.2.3`. A failed
 *  chain shows however far it got, then the failure token. */
export function chainSummary(chain: DnsChain): string {
  const steps = chain.hops.flatMap((hop) => hop.answers.map((a) => recordValue(a.record)));
  const path = [chain.query, ...steps].join(' → ');
  return chain.failure ? `${path} (${chain.failure.kind})` : path;
}
