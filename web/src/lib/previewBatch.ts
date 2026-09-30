// SPDX-License-Identifier: AGPL-3.0-only
// Which import-preview requests to send for the addresses the setup cells asked about in one tick
// (ADR-179 Inc.8). Each cell asks for its own address on mount; sent one by one, a 100-row page was
// 100 POSTs — and every POST under /api/v1 is an audit row. Gathered here, it is one.

import { SWEEP_LIMIT } from './cidr';

/** The addresses not yet asked about, once each and in the order they arrived, cut into requests
 *  of at most `limit` (the endpoint's own cap). Empty when there is nothing new to ask. */
export function previewBatches(
  pending: readonly string[],
  asked: ReadonlySet<string>,
  limit: number = SWEEP_LIMIT,
): string[][] {
  const fresh: string[] = [];
  const seen = new Set<string>();
  for (const ip of pending) {
    if (!ip || asked.has(ip) || seen.has(ip)) continue;
    seen.add(ip);
    fresh.push(ip);
  }
  const out: string[][] = [];
  for (let i = 0; i < fresh.length; i += limit) out.push(fresh.slice(i, i + limit));
  return out;
}
