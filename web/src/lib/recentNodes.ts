// SPDX-License-Identifier: AGPL-3.0-only
// The nodes this browser opened last (ADR-251 decision 11): one of the ways Investigate offers to
// pick what a question is about. Kept per browser on purpose — it is a convenience, so losing it to
// a private window or cleared site data costs a click, never a decision.

/** The most nodes remembered. */
export const RECENT_NODES_MAX = 8;
const KEY = 'yagra.recentNodes';

/** `id` first, the rest after it in their order, without a repeat, at most `max`. */
export function pushRecent(list: readonly string[], id: string, max = RECENT_NODES_MAX): string[] {
  return [id, ...list.filter((x) => x !== id)].slice(0, max);
}

/** Read back a stored value, keeping only strings. Anything else reads as nothing remembered. */
export function parseRecent(raw: string | null): string[] {
  if (!raw) return [];
  try {
    const v: unknown = JSON.parse(raw);
    return Array.isArray(v)
      ? v.filter((x): x is string => typeof x === 'string').slice(0, RECENT_NODES_MAX)
      : [];
  } catch {
    return [];
  }
}

/** The nodes this browser opened, newest first. */
export function readRecentNodes(): string[] {
  try {
    return parseRecent(localStorage.getItem(KEY));
  } catch {
    return [];
  }
}

/** Remember that a node's page was opened. */
export function rememberNode(id: string): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(pushRecent(readRecentNodes(), id)));
  } catch {
    // Storage refused (a private window, blocked site data): nothing is remembered.
  }
}
