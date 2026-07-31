// SPDX-License-Identifier: AGPL-3.0-only
// Pure helpers for poll-pool assignment (ADR-009/020): the node-detail "Pool"/"Polled by" facts,
// pool-name validation shared by the node and folder forms, and the folder-inheritance preview.
// Kept out of the components so all of it is unit-testable (the web Vitest config only picks up
// `.ts` tests, never `.tsx`).

import type { TFunction } from 'i18next';
import type { NodeAssignment, NodeGroup, PolledBy, PoolOption } from '../types/api';
import { isValidPollerToken } from './pollers';

/** Longest accepted pool name — one NATS subject token, matching `MAX_POOL_LEN` in yagra-core. */
export const MAX_POOL_LEN = 63;

/** How many pool chips the tree's context menu shows before relying on "Custom…". Kept small so
 *  the menu stays a menu; the Custom… chip is always rendered, so nothing becomes unreachable. */
export const POOL_CHIP_LIMIT = 8;

/** One chip in the context menu's pool row. */
export interface PoolChoice {
  name: string;
  /** A live poller serves this pool (else assigning to it leaves the node unmonitored). */
  live: boolean;
  /** This is the target's current own pool — the chip renders as already-selected. */
  current: boolean;
}

/** The pool chips to offer for a target whose own pool is `current` (`null`/`''` ⇒ inherited).
 *
 *  The current pool is always included and always first even if it fell out of the server's list
 *  (e.g. its last poller went away and nothing else references it) — otherwise the menu would
 *  silently fail to show what the node is actually set to. The rest keep the server's order,
 *  capped at [`POOL_CHIP_LIMIT`]. */
export function poolChoices(
  pools: PoolOption[],
  current: string | null | undefined,
): PoolChoice[] {
  const own = current?.trim() || null;
  const seen = new Set<string>();
  const out: PoolChoice[] = [];
  if (own) {
    seen.add(own);
    out.push({ name: own, live: pools.some((p) => p.name === own && p.live), current: true });
  }
  for (const p of pools) {
    if (out.length >= POOL_CHIP_LIMIT) break;
    if (seen.has(p.name)) continue;
    seen.add(p.name);
    out.push({ name: p.name, live: p.live, current: false });
  }
  return out;
}

/** Whether a pool name is acceptable. Same alphabet as a poller id (it becomes the
 *  `yagra.jobs.<pool>` subject token), plus the server's length bound. Empty is **valid** here and
 *  means "inherit" — the forms send `''` to clear an assignment. */
export function isValidPoolName(value: string): boolean {
  const trimmed = value.trim();
  if (trimmed === '') return true;
  return trimmed.length <= MAX_POOL_LEN && isValidPollerToken(trimmed);
}

/** Human label for the "Polled by" fact. Only `assigned` names a poller; every other state is a
 *  distinct operational condition worth spelling out (see `PolledBy`). `t` is threaded from the
 *  caller so the label follows the active language. */
export function polledByLabel(polledBy: PolledBy | undefined, t: TFunction): string {
  if (!polledBy) return '—';
  switch (polledBy.state) {
    case 'assigned':
      return polledBy.poller_id ?? '—';
    case 'legacy_fanout':
      return t('nodes:field.polledByLegacy');
    case 'pending':
      return t('nodes:field.polledByPending');
    case 'meraki':
      return t('nodes:field.polledByMeraki');
    default:
      return t('nodes:field.polledByUnknown');
  }
}

/** Whether the "Polled by" state should read as a problem rather than as information. Only
 *  `legacy_fanout` qualifies: the pool has no live poller, so its jobs are published to a subject
 *  nothing is subscribed to and the node is probably unmonitored. */
export function polledByIsWarning(polledBy: PolledBy | undefined): boolean {
  return polledBy?.state === 'legacy_fanout';
}

/** Human label for the "Pool" fact: the effective pool, annotated when it was inherited rather than
 *  set on the node. `groupName` resolves the supplying folder (caller has the group tree). */
export function poolFactLabel(
  assignment: NodeAssignment | undefined,
  groupName: (id: string) => string | undefined,
  t: TFunction,
): string {
  if (!assignment) return '—';
  const { pool, pool_source, pool_source_group_id } = assignment;
  if (pool_source === 'group') {
    const from = (pool_source_group_id && groupName(pool_source_group_id)) || undefined;
    return from ? t('nodes:field.poolInheritedFrom', { pool, group: from }) : pool;
  }
  if (pool_source === 'default') return t('nodes:field.poolDefault', { pool });
  return pool;
}

/** The pool a folder's nodes would inherit from its **ancestors** — i.e. ignoring the folder's own
 *  value. Drives the "inherits X" placeholder in the folder form.
 *
 *  This deliberately mirrors yagra-core's `poolres::PoolResolver`, so keep it to form previews:
 *  the node-detail Pool fact must always come from `getNodeAssignment`, leaving the backend the
 *  single authority on what actually polls a node. Returns `undefined` when nothing is inherited.
 *  Bounded by the group count so malformed (cyclic) data can't loop forever. */
export function inheritedGroupPool(
  groups: NodeGroup[],
  parentId: string | null | undefined,
): string | undefined {
  const byId = new Map(groups.map((g) => [g.id, g]));
  let cur = parentId ?? null;
  for (let i = 0; i <= groups.length && cur; i += 1) {
    const g = byId.get(cur);
    if (!g) return undefined;
    const own = g.pool?.trim();
    if (own) return own;
    cur = g.parent_id ?? null;
  }
  return undefined;
}
