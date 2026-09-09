// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { PoolSummary } from '../types/api';
import i18n from '../i18n';
import {
  buildPollerEnv,
  isValidPollerToken,
  lastSeenLabel,
  poolHasWarning,
  poolTakeoverActions,
  poolModeLabel,
  workingSetLabel,
  POLLER_UP_COMMAND,
} from './pollers';

// English is bundled synchronously (see i18n.ts), so `t` resolves the `system:` keys the label
// helpers use; the assertions below check the resolved English strings.
const t = i18n.t.bind(i18n);

const pool = (over: Partial<PoolSummary> = {}): PoolSummary => ({
  pool: 'tokyo',
  nodes: 10,
  live_pollers: 1,
  mode: 'working_set',
  warning: null,
  ...over,
});

describe('poolHasWarning', () => {
  it('is true only for the nodes_without_live_poller warning', () => {
    expect(poolHasWarning(pool({ warning: 'nodes_without_live_poller' }))).toBe(true);
    expect(poolHasWarning(pool({ warning: null }))).toBe(false);
  });
});

describe('poolModeLabel', () => {
  it('humanizes the dispatch mode', () => {
    expect(poolModeLabel('working_set', t)).toBe('Working set');
    expect(poolModeLabel('legacy', t)).toBe('Legacy');
  });
});

describe('workingSetLabel', () => {
  it('summarizes nodes and specs, singularizing 1', () => {
    expect(workingSetLabel(5, 9, true, t)).toBe('5 nodes / 9 specs');
    expect(workingSetLabel(1, 1, true, t)).toBe('1 node / 1 spec');
    expect(workingSetLabel(0, 0, true, t)).toBe('0 nodes / 0 specs');
  });

  it('renders an em dash for an offline poller (its counts are zeroes)', () => {
    expect(workingSetLabel(0, 0, false, t)).toBe('—');
  });
});

describe('lastSeenLabel', () => {
  const now = Date.parse('2026-07-06T01:00:00Z');

  it('shows relative time when a timestamp exists', () => {
    expect(lastSeenLabel('2026-07-06T00:55:00Z', true, t, now)).toBe('5m ago');
  });

  it('shows Live for an online poller not yet persisted, em dash when offline', () => {
    expect(lastSeenLabel(null, true, t, now)).toBe('Live');
    expect(lastSeenLabel(null, false, t, now)).toBe('—');
  });
});

describe('isValidPollerToken', () => {
  it('accepts the server alphabet and rejects everything else', () => {
    expect(isValidPollerToken('tokyo-edge_1')).toBe(true);
    expect(isValidPollerToken('POOL9')).toBe(true);
    expect(isValidPollerToken('')).toBe(false);
    expect(isValidPollerToken('has space')).toBe(false);
    expect(isValidPollerToken('dots.are.out')).toBe(false);
    expect(isValidPollerToken('slash/no')).toBe(false);
  });
});

describe('buildPollerEnv', () => {
  it('emits the three required vars in order', () => {
    expect(
      buildPollerEnv({ id: 'tokyo-edge-1', pool: 'tokyo', busUrl: 'tls://poller:pw@bus:4222' }),
    ).toBe(
      ['YAGRA_POLLER_ID=tokyo-edge-1', 'YAGRA_POLLER_POOL=tokyo', 'YAGRA_BUS_URL=tls://poller:pw@bus:4222'].join(
        '\n',
      ),
    );
  });

  it('appends the CA line only when a path is given', () => {
    const withCa = buildPollerEnv({
      id: 'e1',
      pool: 'p',
      busUrl: 'tls://bus:4222',
      caFile: '/certs/ca.pem',
    });
    expect(withCa).toContain('YAGRA_BUS_CA_FILE=/certs/ca.pem');

    const blank = buildPollerEnv({ id: 'e1', pool: 'p', busUrl: 'tls://bus:4222', caFile: '   ' });
    expect(blank).not.toContain('YAGRA_BUS_CA_FILE');
  });
});

describe('POLLER_UP_COMMAND', () => {
  it('targets the remote-poller compose file', () => {
    expect(POLLER_UP_COMMAND).toBe('docker compose -f docker-compose.poller.yml up -d');
  });
});

describe('poolTakeoverActions', () => {
  it('offers the takeover only for a pool that has members and nothing polling them', () => {
    expect(poolTakeoverActions(pool({ warning: 'nodes_without_live_poller', live_pollers: 0 })))
      .toEqual({ cover: true, restore: false });
    expect(poolTakeoverActions(pool({ warning: null }))).toEqual({ cover: false, restore: false });
  });

  it('offers only the restore once a pool is covered, never both', () => {
    // A covered pool reads 0 nodes and 0 live pollers, so it carries no warning either — but pin
    // both halves, because "cover" on an already-covered pool would lose the first `previous_pool`.
    expect(poolTakeoverActions(pool({ covered_by: 'default', nodes: 0, live_pollers: 0 })))
      .toEqual({ cover: false, restore: true });
    expect(poolTakeoverActions(pool({ covered_by: 'default', warning: 'nodes_without_live_poller' })))
      .toEqual({ cover: false, restore: true });
  });

  // 🚨 The bug this helper exists to prevent, found on hardware 2026-09-09. The page asked
  // `p.warning &&` — truthy — while the card's pill asked `poolHasWarning`, which tests for one
  // token. They agree today because `warning` has exactly one non-null value, so no test that used
  // a REAL warning could tell them apart. This one uses a warning that is not that token: the loose
  // form says "cover", the correct form says nothing.
  it('is silent for a warning that is not the no-live-poller one', () => {
    const other = pool({ warning: 'some_future_warning' as never, live_pollers: 3 });
    expect(poolHasWarning(other)).toBe(false);
    expect(poolTakeoverActions(other)).toEqual({ cover: false, restore: false });
  });
});
