// SPDX-License-Identifier: AGPL-3.0-only
// Pure helpers for managing poller pools from Settings ▸ Pollers (ADR-107).
//
// Separate from `lib/pool.ts`, which answers "which pool is this *node* in" for the inventory tree.
// This file answers "which pollers serve this pool, and what is still pending" — the poller side of
// the same word. Both are `.ts` rather than `.tsx` because Vitest runs in a node environment and
// never executes a `.tsx` test (testing.md), and every judgement below is one a component would
// otherwise hide.

import type { PollerInfo } from '../types/api';

/** The line a site has to change for a recorded move to take effect. */
export function poolEnvLine(pool: string): string {
  return `YAGRA_POLLER_POOL=${pool}`;
}

/**
 * Which pools this poller answers to.
 *
 * **Either it reports the pool, or it is recorded as heading there.** Somebody who selects `tokyo`
 * is asking which pollers will serve tokyo, so a poller on its way is part of that answer — and the
 * row carries its own "pending" badge, so including it cannot be mistaken for having arrived.
 *
 * ⚠️ The consequence is that a poller with a pending move appears under **both** pools. That is the
 * truth: it is serving the first and has been told to serve the second.
 *
 * 🚨 **This is the only implementation of that rule.** `pollerFilters.ts` reads the pool column
 * through it, so the cards in the strip and the column filter cannot disagree — they are not two
 * mechanisms that happen to agree, they are one state written from two places (ui-conventions:
 * a second control editing one state is how a filter forks).
 */
export function poolValuesOf(p: PollerInfo): string[] {
  const desired = p.desired_pool;
  return desired && desired !== p.pool ? [p.pool, desired] : [p.pool];
}

/** Does this poller answer to `pool`? */
export function pollerInPool(p: PollerInfo, pool: string): boolean {
  return poolValuesOf(p).includes(pool);
}

/**
 * How many of `shown` are there only because of a pending move — the number the count line
 * discloses.
 *
 * Without it, "ポーラー 2 台" claims a pool has two pollers when one of them is still serving
 * somewhere else, which is exactly the kind of quietly-wrong number an operator plans capacity from.
 * Takes the selected set rather than one name, because the pool filter is multi-select.
 */
export function pendingArrivals(shown: readonly PollerInfo[], pools: readonly string[]): number {
  if (pools.length === 0) return 0;
  return shown.filter((p) => !pools.includes(p.pool)).length;
}

/** A poller has a move recorded that has not taken effect yet. */
export function hasPendingMove(p: PollerInfo): boolean {
  return !!p.desired_pool && p.desired_pool !== p.pool;
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
 * Pollers blocking a rename — the ones *reporting* the name.
 *
 * 🚨 Narrower than {@link poolUsage} on purpose. A poller merely recorded as heading to a name does
 * not block renaming it away, because its `.env` has not been changed yet either; one that reports
 * it does, because a rename moves nodes and folders and cannot move the poller. Leave it and the
 * old name stays live while the new name's nodes drop into legacy fan-out, where their jobs go to a
 * subject nobody subscribes to and are silently discarded.
 */
export function renameBlockers(pool: string, rows: readonly PollerInfo[]): string[] {
  return rows.filter((p) => p.pool === pool).map((p) => p.id);
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
 * ancestor carries the whole string. It caught exactly that here: with a long destination the
 * badges were cut by 41px and 132px with nothing to hover. So this must contain **both pool names
 * and the pending label**, not a paraphrase of them.
 *
 * `pending` is the localized badge text; `hint` is the sentence explaining the state.
 */
export function poolCellTitle(
  p: PollerInfo,
  labels: { pending: string; hint: string },
): string {
  const head = hasPendingMove(p)
    ? `${p.pool} → ${p.desired_pool} (${labels.pending})`
    : p.pool;
  return `${head} — ${labels.hint}`;
}
