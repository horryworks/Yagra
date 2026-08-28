// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { PollerInfo } from '../types/api';
import {
  isValidNewPoolName,
  moveEmptiesSourcePool,
  newPoolIsIdle,
  poolCellTitle,
  poolIsRemovable,
  poolUsage,
  poolValuesOf,
  pollerCanMove,
  pollerInPool,
  renamePassengers,
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
    can_change_pool: true,
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
  poller('core2', 'default'),
  poller('tok1', 'tokyo'),
  poller('osa1', 'osaka', { status: 'offline', can_change_pool: false }),
];

/** The real filter, driven the way a pool card drives it: one token in the `pool` column. */
function shownFor(pool: string | null) {
  const cols = Object.entries(
    pollerFilters(((k: string) => k) as never, ['default', 'tokyo', 'osaka', 'lab']),
  ).map(([key, filter]) => ({ key, filter }));
  return applyFilters(FLEET, cols, { pool: pool ?? '' }, Date.now());
}

describe('pool membership', () => {
  it('shows every poller when nothing is selected', () => {
    expect(shownFor(null).map((p) => p.id)).toEqual(['aio1', 'core2', 'tok1', 'osa1']);
  });

  it('narrows to one pool', () => {
    expect(shownFor('default').map((p) => p.id)).toEqual(['aio1', 'core2']);
  });

  it('is empty for a pool nothing serves yet', () => {
    expect(shownFor('lab').map((p) => p.id)).toEqual([]);
  });

  it('answers membership both ways round', () => {
    const p = poller('x', 'tokyo');
    expect(pollerInPool(p, 'tokyo')).toBe(true);
    expect(pollerInPool(p, 'default')).toBe(false);
  });

  /**
   * A poller is in exactly one pool since ADR-107 Inc.2 — a move is instantaneous, so there is no
   * "recorded as heading there" state to represent any more. Kept as a test because the *shape*
   * matters: `pollerFilters.ts` reads the pool column through `poolValuesOf`, so the cards in the
   * strip and the column filter cannot disagree.
   */
  it('lists exactly the pool a poller serves', () => {
    expect(poolValuesOf(poller('x', 'tokyo'))).toEqual(['tokyo']);
  });
});

describe('pool names are one subject token', () => {
  it('accepts what an operator types', () => {
    expect(isValidNewPoolName('tokyo')).toBe(true);
    expect(isValidNewPoolName('east-dc_2')).toBe(true);
    expect(isValidNewPoolName('  padded  ')).toBe(true);
  });

  it('refuses anything that would partition the NATS subject or overflow the token', () => {
    expect(isValidNewPoolName('tokyo.1')).toBe(false);
    expect(isValidNewPoolName('east dc')).toBe(false);
    expect(isValidNewPoolName('a*b')).toBe(false);
    expect(isValidNewPoolName('x'.repeat(64))).toBe(false);
    expect(isValidNewPoolName('x'.repeat(63))).toBe(true);
  });

  it('refuses blank — naming a new pool has no "inherit" case', () => {
    expect(isValidNewPoolName('')).toBe(false);
    expect(isValidNewPoolName('   ')).toBe(false);
  });
});

describe('what blocks removing a pool, and what a rename carries', () => {
  it('reports every poller that names it', () => {
    const u = poolUsage('default', 32, FLEET);
    expect(u.pollers).toEqual(['aio1', 'core2']);
    expect(u.livePollers).toBe(2);
    expect(u.nodes).toBe(32);
    expect(poolIsRemovable(u)).toBe(false);
  });

  it('removes a pool nothing points at', () => {
    const u = poolUsage('lab', 0, FLEET);
    expect(u.pollers).toEqual([]);
    expect(poolIsRemovable(u)).toBe(true);
  });

  it('refuses removal for nodes alone, with no poller in sight', () => {
    const u = poolUsage('lab', 4, FLEET);
    expect(u.pollers).toEqual([]);
    expect(poolIsRemovable(u)).toBe(false);
  });

  /**
   * 🚨 The list changed meaning in ADR-107 Inc.2 and the name changed with it: these pollers are
   * carried by the rename, not blocking it. A rename re-points `pollers.pool` in the same
   * transaction as the nodes and folders.
   */
  it('names the pollers a rename will carry with it', () => {
    expect(renamePassengers('default', FLEET)).toEqual(['aio1', 'core2']);
    expect(renamePassengers('lab', FLEET)).toEqual([]);
  });
});

describe('whether a poller can be moved at all', () => {
  /**
   * 🚨 Read the server's derived answer, never `caps`. It is false for an offline poller *and* for
   * an older build, and the UI needs the same reply for both: there is nothing to offer.
   */
  it('follows the server, for both reasons it can say no', () => {
    expect(pollerCanMove(poller('new', 'default'))).toBe(true);
    expect(pollerCanMove(poller('old', 'default', { can_change_pool: false }))).toBe(false);
    expect(pollerCanMove(poller('off', 'default', { status: 'offline', can_change_pool: false }))).toBe(
      false,
    );
  });
});

describe('a move that would strand the pool it leaves', () => {
  const nodes = (counts: Record<string, number>) => (pool: string) => counts[pool] ?? 0;

  /**
   * 🚨 The quietest failure in the product: a pool with no live poller falls back to legacy
   * per-job publish on a subject nobody subscribes to, plain NATS discards the jobs, the nodes
   * decay to `unknown` rather than `down`, and every dashboard reads calm.
   */
  it('is caught when the last live poller leaves nodes behind', () => {
    const fleet = [poller('only', 'tokyo'), poller('other', 'default')];
    expect(
      moveEmptiesSourcePool(fleet[0], 'default', fleet, nodes({ tokyo: 40 })),
    ).toEqual({ pool: 'tokyo', nodes: 40 });
  });

  it('is not raised while another live poller stays behind', () => {
    const fleet = [poller('a', 'tokyo'), poller('b', 'tokyo')];
    expect(moveEmptiesSourcePool(fleet[0], 'default', fleet, nodes({ tokyo: 40 }))).toBeNull();
  });

  /** An offline poller cannot poll anything, so it does not count as cover. */
  it('ignores an offline poller when asking who is left', () => {
    const fleet = [poller('a', 'tokyo'), poller('b', 'tokyo', { status: 'offline' })];
    expect(
      moveEmptiesSourcePool(fleet[0], 'default', fleet, nodes({ tokyo: 40 })),
    ).toEqual({ pool: 'tokyo', nodes: 40 });
  });

  /**
   * Nothing goes dark when there is nothing to poll, and asking anyway would be a dialog the
   * operator cannot act on. The API's refusal uses the same trigger for exactly that reason.
   */
  it('is not raised for a pool with no nodes', () => {
    const fleet = [poller('only', 'tokyo')];
    expect(moveEmptiesSourcePool(fleet[0], 'default', fleet, nodes({}))).toBeNull();
  });

  it('is not raised for a move to the pool it is already in', () => {
    const fleet = [poller('only', 'tokyo')];
    expect(moveEmptiesSourcePool(fleet[0], 'tokyo', fleet, nodes({ tokyo: 40 }))).toBeNull();
  });
});

describe('a freshly created pool is idle, not broken', () => {
  it('is idle with nothing assigned and nothing serving', () => {
    expect(newPoolIsIdle(0, 0)).toBe(true);
  });

  it('is not idle once nodes are assigned to it', () => {
    expect(newPoolIsIdle(4, 0)).toBe(false);
  });

  it('is not idle once a poller serves it', () => {
    expect(newPoolIsIdle(0, 1)).toBe(false);
  });
});

describe('the pool cell says everything it draws', () => {
  /**
   * 🚨 ADR-088's geometry check refuses clipped text unless a `title` carries the whole string, and
   * it caught exactly this cell: a long pool name was cut with nothing to hover. So the title must
   * contain the name verbatim, never a paraphrase of it.
   */
  it('carries the pool name verbatim, however long', () => {
    const long = 'ymock-a-very-long-pool-name-that-will-not-fit';
    const title = poolCellTitle(poller('p', long), { hint: 'Move this poller' });
    expect(title).toContain(long);
    expect(title).toContain('Move this poller');
  });
});
