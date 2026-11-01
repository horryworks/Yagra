// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { ConfigBackupRow } from '../types/api';
import { countsOf, planSwitch, rowsIn, stateOf } from './configBackupList';

function row(id: string, over: Partial<ConfigBackupRow>): ConfigBackupRow {
  return {
    node_id: id,
    enabled: false,
    configured: false,
    model: 'ios',
    input: null,
    credential_id: null,
    login_confirmed: false,
    pending: false,
    try_after: null,
    last: null,
    last_success_at: null,
    poller: 'ready',
    ...over,
  };
}

const running = row('a', { enabled: true, configured: true, input: 'ssh' });
const stopped = row('b', { configured: true, input: 'telnet' });
const fresh = row('c', {});
const noModel = row('d', { configured: true, input: 'ssh', model: null });

describe('config backup list', () => {
  it('puts each row in one state and counts them', () => {
    expect([running, stopped, fresh].map(stateOf)).toEqual(['on', 'off', 'never']);
    expect(countsOf([running, stopped, fresh, noModel])).toEqual({ on: 1, off: 2, never: 1, all: 4 });
    expect(rowsIn([running, stopped, fresh], 'off')).toEqual([stopped]);
    expect(rowsIn([running, stopped, fresh], 'all')).toHaveLength(3);
  });

  it('switches off only what is on', () => {
    expect(planSwitch([running, stopped], false)).toEqual({ ids: ['a'], skipped: [], needsInput: false });
  });

  it('switches on what has a model, and asks for an input only for a node with no settings', () => {
    expect(planSwitch([running, stopped], true)).toEqual({ ids: ['b'], skipped: [], needsInput: false });
    const plan = planSwitch([stopped, fresh, noModel], true);
    expect(plan.ids).toEqual(['b', 'c']);
    expect(plan.skipped).toEqual([noModel]);
    expect(plan.needsInput).toBe(true);
  });
});
