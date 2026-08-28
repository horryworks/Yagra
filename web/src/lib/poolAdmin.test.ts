// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { PollerInfo } from '../types/api';
import {
  hasPendingMove,
  isValidNewPoolName,
  newPoolIsIdle,
  pendingArrivals,
  poolCellTitle,
  poolEnvLine,
  poolIsRemovable,
  poolUsage,
  poolValuesOf,
  pollerInPool,
  renameBlockers,
} from './poolAdmin';
import { applyFilters } from './filterPredicate';
import { pollerFilters } from '../pages/pollerFilters';

/** A poller row with only the fields these helpers read. */
function poller(
  id: string,
  pool: string,
  extra: Partial<PollerInfo> = {},
): PollerInfo {
  return {
    id,
    pool,
    desired_pool: null,
    status: 'online',
    last_seen: null,
    first_seen: null,
    version: null,
    working_set_nodes: 0,
    working_set_specs: 0,
    results_total: 0,
    cpu_pct: null,
    mem_used_pct: null,
    disk_used_pct: null,
    mgmt_addrs: [],
    anchor_node_id: null,
    caps: [],
    listeners: [],
    has_token: false,
    token_issued_at: null,
    upgrade: null,
    ...extra,
  } as PollerInfo;
}

const FLEET = [
  poller('aio1', 'default'),
  poller('core2', 'default', { desired_pool: 'tokyo' }),
  poller('tok1', 'tokyo'),
  poller('osa1', 'osaka', { status: 'offline' }),
];

/** The real filter, driven the way a pool card drives it: one token in the `pool` column. */
function shownFor(pool: string | null) {
  const cols = Object.entries(
    pollerFilters(((k: string) => k) as never, ['default', 'tokyo', 'osaka', 'lab-move']),
  ).map(([key, filter]) => ({ key, filter }));
  return applyFilters(FLEET, cols, { pool: pool ?? '' }, Date.now());
}

describe('pool membership', () => {
  it('shows every poller when nothing is selected', () => {
    expect(shownFor(null).map((p) => p.id)).toEqual(['aio1', 'core2', 'tok1', 'osa1']);
  });

  it('narrows to one pool', () => {
    expect(shownFor('osaka').map((p) => p.id)).toEqual(['osa1']);
  });

  // The design decision, pinned **through the real filter** rather than through a helper that only
  // the test calls: a card sets the same `pool` filter the column does, so if the two ever forked
  // this assertion is what notices.
  it('includes a poller recorded as heading to the pool', () => {
    expect(shownFor('tokyo').map((p) => p.id)).toEqual(['core2', 'tok1']);
  });

  // ...and it therefore appears under both. That is the truth, not a bug.
  it('keeps a pending poller under the pool it is still serving', () => {
    expect(shownFor('default').map((p) => p.id)).toEqual(['aio1', 'core2']);
  });

  it('is empty for a pool nothing serves yet', () => {
    expect(shownFor('lab-move')).toEqual([]);
  });

  it('answers membership both ways round', () => {
    const c = FLEET[1];
    expect(poolValuesOf(c)).toEqual(['default', 'tokyo']);
    expect(pollerInPool(c, 'default')).toBe(true);
    expect(pollerInPool(c, 'tokyo')).toBe(true);
    expect(pollerInPool(c, 'osaka')).toBe(false);
  });

  it('lists one pool for a poller with no move pending', () => {
    expect(poolValuesOf(FLEET[0])).toEqual(['default']);
  });
});

describe('the count line discloses pending arrivals', () => {
  it('counts only the ones not yet serving the pool', () => {
    const shown = shownFor('tokyo');
    expect(shown).toHaveLength(2);
    expect(pendingArrivals(shown, ['tokyo'])).toBe(1);
  });

  it('counts nothing when every shown poller is already there', () => {
    expect(pendingArrivals(shownFor('osaka'), ['osaka'])).toBe(0);
  });

  it('counts nothing with no selection — the unfiltered list makes no claim about a pool', () => {
    expect(pendingArrivals(FLEET, [])).toBe(0);
  });

  it('counts across a multi-pool selection, since the filter is a set', () => {
    // Selecting default AND tokyo: core2 reports default, so it is not pending for that selection.
    expect(pendingArrivals(FLEET, ['default', 'tokyo'])).toBe(1);
  });
});

describe('a pending move', () => {
  it('is recorded only when the destination differs from where the poller is', () => {
    expect(hasPendingMove(FLEET[1])).toBe(true);
    expect(hasPendingMove(FLEET[0])).toBe(false);
    // A record naming the pool it already serves is not pending — this is the state the heartbeat
    // clears, and the badge must not flicker on in the window before it does.
    expect(hasPendingMove(poller('x', 'tokyo', { desired_pool: 'tokyo' }))).toBe(false);
  });
});

describe('pool names are one subject token', () => {
  it('accepts what an operator types', () => {
    expect(isValidNewPoolName('tokyo')).toBe(true);
    expect(isValidNewPoolName(' edge-01_a ')).toBe(true);
    expect(isValidNewPoolName('x'.repeat(63))).toBe(true);
  });

  it('refuses anything that would partition the NATS subject or overflow the token', () => {
    // `yagra.jobs.tokyo.1` is subscribed by nothing, and plain NATS discards rather than queues.
    expect(isValidNewPoolName('tokyo.1')).toBe(false);
    expect(isValidNewPoolName('east dc')).toBe(false);
    expect(isValidNewPoolName('x'.repeat(64))).toBe(false);
  });

  it('refuses blank — naming a new pool has no "inherit" case', () => {
    expect(isValidNewPoolName('')).toBe(false);
    expect(isValidNewPoolName('   ')).toBe(false);
  });
});

describe('what blocks removing or renaming a pool', () => {
  it('reports every poller that names it, in either direction', () => {
    const u = poolUsage('tokyo', 40, FLEET);
    expect(u.nodes).toBe(40);
    expect(u.pollers).toEqual(['core2', 'tok1']);
    // Live counts only the ones actually serving it and online.
    expect(u.livePollers).toBe(1);
    expect(poolIsRemovable(u)).toBe(false);
  });

  it('removes a pool nothing points at', () => {
    const u = poolUsage('lab-move', 0, FLEET);
    expect(u.pollers).toEqual([]);
    expect(poolIsRemovable(u)).toBe(true);
  });

  it('refuses removal for nodes alone, with no poller in sight', () => {
    expect(poolIsRemovable(poolUsage('lab-move', 3, FLEET))).toBe(false);
  });

  // 🚨 The asymmetry that matters: a rename is blocked by a poller *reporting* the name, not by one
  // merely heading there. Getting this wrong in the lenient direction opens a monitoring hole; in
  // the strict direction it merely refuses a rename that would have been safe.
  it('blocks a rename only on pollers that report the name', () => {
    expect(renameBlockers('tokyo', FLEET)).toEqual(['tok1']);
    expect(renameBlockers('default', FLEET)).toEqual(['aio1', 'core2']);
    expect(renameBlockers('lab-move', FLEET)).toEqual([]);
  });

  it('lets a pool be renamed away from a poller that has only been told to go there', () => {
    // `core2` is recorded as heading to tokyo but still serves default, so its `.env` still says
    // default — renaming tokyo cannot strand it.
    const blockers = renameBlockers('tokyo', [poller('core2', 'default', { desired_pool: 'tokyo' })]);
    expect(blockers).toEqual([]);
  });
});

describe('a freshly created pool is idle, not broken', () => {
  it('is idle with nothing assigned and nothing serving', () => {
    expect(newPoolIsIdle(0, 0)).toBe(true);
  });

  // The distinction the strip draws: nodes with no live poller is the real warning, because those
  // nodes' jobs are being published to a subject nobody subscribes to and discarded.
  it('is not idle once nodes are assigned to it', () => {
    expect(newPoolIsIdle(12, 0)).toBe(false);
  });

  it('is not idle once a poller serves it', () => {
    expect(newPoolIsIdle(0, 1)).toBe(false);
  });
});

describe('the line a site has to change', () => {
  it('is the env var the poller reads at startup', () => {
    expect(poolEnvLine('tokyo')).toBe('YAGRA_POLLER_POOL=tokyo');
  });
});

describe('the pool cell says everything it draws', () => {
  const labels = { pending: 'Move pending', hint: 'Change pool' };

  // 🚨 ADR-088's geometry check found this: a long destination clipped both badges with nothing to
  // hover. The title must therefore carry the *whole* rendered string, not a paraphrase — so these
  // assertions name each substring the cell draws.
  it('carries both pool names and the pending label while a move is recorded', () => {
    const title = poolCellTitle(
      poller('core2', 'default', { desired_pool: 'a-very-long-destination-pool-name' }),
      labels,
    );
    expect(title).toContain('default');
    expect(title).toContain('a-very-long-destination-pool-name');
    expect(title).toContain('Move pending');
    expect(title).toContain('Change pool');
  });

  it('carries just the pool and the hint when nothing is pending', () => {
    const title = poolCellTitle(poller('aio1', 'default'), labels);
    expect(title).toContain('default');
    expect(title).not.toContain('Move pending');
  });

  // A record naming the pool it already serves draws no arrow, so the title must not claim one.
  it('draws no arrow for a record that has already been fulfilled', () => {
    expect(poolCellTitle(poller('x', 'tokyo', { desired_pool: 'tokyo' }), labels)).not.toContain('→');
  });
});
