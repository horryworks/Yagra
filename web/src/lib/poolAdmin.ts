// SPDX-License-Identifier: AGPL-3.0-only
// Pure helpers for managing poller pools from Settings ▸ Pollers (ADR-107).
//
// Separate from `lib/pool.ts`, which answers "which pool is this *node* in" for the inventory tree.
// This file answers "which pollers serve this pool, and what is still pending" — the poller side of
// the same word. Both are `.ts` rather than `.tsx` because Vitest runs in a node environment and
// never executes a `.tsx` test (testing.md), and every judgement below is one a component would
// otherwise hide.

import type { PollerInfo } from '../types/api';

/**
 * Which pools this poller answers to.
 *
 * Exactly one since ADR-107 Inc.2 — the pool core has it serving. Before that a poller could also
 * be *recorded as heading* somewhere, and this returned both; a move is now instantaneous, so there
 * is no in-between state left to represent.
 *
 * 🚨 **Kept as a function anyway, because it is the only implementation of the rule.**
 * `pollerFilters.ts` reads the pool column through it, so the cards in the strip and the column
 * filter cannot disagree — they are not two mechanisms that happen to agree, they are one state
 * written from two places (ui-conventions: a second control editing one state is how a filter
 * forks). Inlining `p.pool` at both call sites is how that comes apart again.
 */
export function poolValuesOf(p: PollerInfo): string[] {
  return [p.pool];
}

/** Does this poller answer to `pool`? */
export function pollerInPool(p: PollerInfo, pool: string): boolean {
  return poolValuesOf(p).includes(pool);
}

/**
 * Is `name` a legal pool name?
 *
 * The name becomes the NATS subject token in `yagra.jobs.{pool}`, so a `.` would partition the
 * subject and the jobs would be published where nothing subscribes — plain NATS, so discarded
 * rather than queued. Same rule as the backend's `validate_pool_name`; the mirror is deliberate
 * (the API is authoritative) and exists so the dialog can refuse before a round trip.
 *
 * ⚠️ Empty is **false** here, unlike `lib/pool.ts::isValidPoolName` where blank means "inherit".
 * There is no inheriting when you are naming a new pool.
 */
export function isValidNewPoolName(name: string): boolean {
  const t = name.trim();
  return t.length > 0 && t.length <= 63 && /^[A-Za-z0-9_-]+$/.test(t);
}

/** What is still pointing at a pool, as the delete refusal reports it. */
export interface PoolUsage {
  nodes: number;
  livePollers: number;
  pollers: string[];
}

/**
 * Work out what still names `pool`, from what the page already loaded.
 *
 * Client-side because both inputs are already in the browser: the pool summaries come with the
 * poller list, and the poller inventory is bounded by how many were deployed rather than by fleet
 * size. The server refuses authoritatively — this is only so the dialog can say *what* before the
 * operator clicks, rather than showing them a 409 afterwards.
 */
export function poolUsage(pool: string, nodes: number, rows: readonly PollerInfo[]): PoolUsage {
  const pollers = rows.filter((p) => pollerInPool(p, pool)).map((p) => p.id);
  return {
    nodes,
    livePollers: rows.filter((p) => p.pool === pool && p.status === 'online').length,
    pollers,
  };
}

/** Nothing names this pool, so removing its description removes the pool. */
export function poolIsRemovable(u: PoolUsage): boolean {
  return u.nodes === 0 && u.pollers.length === 0;
}

/**
 * Pollers a rename will carry with it — the ones serving the name.
 *
 * 🚨 **This used to be a list of blockers and is now a list of passengers**, and the difference is
 * ADR-107 Inc.2. A rename moves nodes and folders; before core owned `pollers.pool` it could not
 * move the poller, so the old name stayed live while the new name's nodes dropped into legacy
 * fan-out and their jobs went to a subject nobody subscribed to. The rename transaction now
 * re-points the pollers too — so the only remaining question is whether each of them *can* follow
 * one, which is {@link pollerCanMove}.
 */
export function renamePassengers(pool: string, rows: readonly PollerInfo[]): string[] {
  return rows.filter((p) => p.pool === pool).map((p) => p.id);
}

/**
 * Can this poller be moved to another pool at all?
 *
 * 🚨 **Read the server's derived answer, never `caps`.** `can_change_pool` is true only for a build
 * that advertises `pool-follow` *and* is online, and both halves matter: an offline poller has
 * nobody to receive the snapshot that tells it where it went, and an older build would take the new
 * pool's working set while leaving "poll now" and discovery pointed at the old pool's subjects —
 * where nothing listens and plain NATS discards them, with no error anywhere.
 *
 * Every poller is `false` immediately after this release until its own build is upgraded, so the
 * UI has to say *why* rather than simply not offering the control.
 */
export function pollerCanMove(p: PollerInfo): boolean {
  return p.can_change_pool;
}

/**
 * Would moving `poller` out of its pool leave that pool with monitored inventory and nobody to
 * poll it? Returns the count that would go dark, or `null` when the move is safe.
 *
 * 🚨 **This decides whether to ask, and nothing more.** The API asks the same question again and
 * refuses with `409 source_pool_would_empty` unless the caller has answered it — so this is not a
 * mirror of that check, it is the other half of a two-part question ("should I show the dialog?"
 * here, "did the caller answer?" there). The server is the authority; a client that skipped this
 * gets the refusal rather than the hole.
 *
 * The failure being prevented is the quietest one in the product: a pool with no live poller falls
 * back to legacy per-job publish on `yagra.jobs.{pool}`, nothing is subscribed, and the jobs are
 * discarded. The nodes decay to `unknown` rather than `down`, every dashboard reads calm, and
 * `pool_coverage` only raises an alert after a five-minute debounce.
 */
export function moveEmptiesSourcePool(
  poller: PollerInfo,
  to: string,
  rows: readonly PollerInfo[],
  nodesInPool: (pool: string) => number,
): { pool: string; nodes: number } | null {
  const from = poller.pool;
  if (!from || from === to) return null;
  const othersLeft = rows.some(
    (p) => p.id !== poller.id && p.pool === from && p.status === 'online',
  );
  if (othersLeft) return null;
  const nodes = nodesInPool(from);
  // 🚨 **Nodes, not folders**, and the server's refusal uses the same trigger deliberately. A
  // folder assigned to the pool with no nodes under it strands nothing today — it only decides
  // where future nodes land — so refusing there would be a dialog nobody could act on. The folder
  // count is still reported by the API and still travels with a `move_nodes` answer.
  return nodes > 0 ? { pool: from, nodes } : null;
}

/**
 * A pool with nodes but no live poller is a warning; a pool with neither is simply not set up yet.
 *
 * ⚠️ Deliberately not the same question as `poolHasWarning` in `lib/pollers.ts`, which reads the
 * server's own verdict for a pool that exists in the summary. This one is asked about a pool the
 * operator has *just created*, where the summary has not caught up — and answering "warning" there
 * would fire an alarm on every new pool the moment it is named.
 */
export function newPoolIsIdle(nodes: number, livePollers: number): boolean {
  return nodes === 0 && livePollers === 0;
}

/**
 * The pool cell's `title` — every string the cell renders, plus what it means.
 *
 * 🚨 **Not decoration.** A pool name may be 63 characters and the column is not, so the cell will
 * sometimes clip — and ADR-088's geometry check refuses clipped text unless a `title` on it or an
 * ancestor carries the whole string. It caught exactly that here once, with the badges cut by 41px
 * and 132px and nothing to hover. So this must contain the pool name verbatim, never a paraphrase.
 */
export function poolCellTitle(p: PollerInfo, labels: { hint: string }): string {
  return `${p.pool} — ${labels.hint}`;
}
