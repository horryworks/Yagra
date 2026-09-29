// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { RediscoverComparison, RediscoverView } from '../../types/api';
import {
  applicable,
  applyBody,
  keepPolling,
  phaseOf,
  PICKUP_WARN_MS,
  refusalKey,
  rowsOf,
} from './rediscoverState';

const comparison = (over: Partial<RediscoverComparison> = {}): RediscoverComparison => ({
  profile_locked: false,
  sys_object_id: '1.3.6.1.4.1.2011.2.23.1',
  sys_descr: 'Huawei VRP',
  sys_name: 'sw-01',
  profile: {
    current_id: 'p-generic',
    current_name: 'Generic SNMP',
    found_id: 'p-huawei',
    found_name: 'Huawei switch',
    rule_id: null,
    verdict: 'differs',
  },
  vendor: { current: null, found: 'Huawei', verdict: 'differs' },
  model: { current: 'CE6881', found: null, verdict: 'undetermined' },
  ...over,
});

const view = (state: RediscoverView['state'], c?: RediscoverComparison): RediscoverView => ({
  scan_id: 's-1',
  state,
  comparison: c ?? null,
});

describe('phaseOf', () => {
  it('never reads waiting or reading as an outcome', () => {
    expect(phaseOf(null, false, 0)).toBe('starting');
    expect(phaseOf(view('waiting'), false, 1_000)).toBe('waiting');
    expect(phaseOf(view('reading'), false, 1_000)).toBe('reading');
    for (const p of ['starting', 'waiting', 'waitingLong', 'reading'] as const) {
      expect(keepPolling(p), p).toBe(true);
    }
  });

  it('says so when no poller has picked the re-read up for a minute', () => {
    expect(phaseOf(view('waiting'), false, PICKUP_WARN_MS - 1)).toBe('waiting');
    expect(phaseOf(view('waiting'), false, PICKUP_WARN_MS)).toBe('waitingLong');
  });

  it('tells the outcomes apart and stops asking on each', () => {
    expect(phaseOf(view('answered', comparison()), false, 0)).toBe('answered');
    expect(phaseOf(view('no_snmp_answer'), false, 0)).toBe('noSnmpAnswer');
    expect(phaseOf(view('no_answer'), false, 0)).toBe('noAnswer');
    expect(phaseOf(view('stopped'), false, 0)).toBe('stopped');
    expect(phaseOf(view('answered', comparison()), true, 0)).toBe('lost');
    for (const p of ['answered', 'noSnmpAnswer', 'noAnswer', 'stopped', 'lost'] as const) {
      expect(keepPolling(p), p).toBe(false);
    }
  });

  it('does not draw rows for an answer that carries none', () => {
    expect(phaseOf(view('answered'), false, 0)).toBe('reading');
  });
});

describe('rows and apply', () => {
  it('names the profile rather than showing its id', () => {
    const [profile] = rowsOf(comparison());
    expect(profile).toMatchObject({ current: 'Generic SNMP', found: 'Huawei switch' });
  });

  it('offers only the rows that differ', () => {
    expect(applicable(comparison())).toEqual(['profile', 'vendor']);
    const locked = comparison({
      profile: { ...comparison().profile, verdict: 'locked' },
    });
    expect(applicable(locked)).toEqual(['vendor']);
  });

  it('echoes what was shown as current for each chosen row', () => {
    expect(applyBody('s-1', comparison(), new Set(['profile', 'vendor', 'model']))).toEqual({
      scan_id: 's-1',
      profile: { from: 'p-generic', to: 'p-huawei' },
      vendor: { from: null, to: 'Huawei' },
    });
  });

  it('never sends a row the person unticked or one that cannot be applied', () => {
    expect(applyBody('s-1', comparison(), new Set(['vendor']))).toEqual({
      scan_id: 's-1',
      vendor: { from: null, to: 'Huawei' },
    });
    expect(applyBody('s-1', comparison(), new Set(['model']))).toBeNull();
    expect(applyBody('s-1', comparison(), new Set())).toBeNull();
  });
});

describe('refusalKey', () => {
  it('words the refusals the dialog can explain, and leaves the rest to the server', () => {
    expect(refusalKey('no_live_poller')).toBe('rediscover.err.noLivePoller');
    expect(refusalKey('node_changed')).toBe('rediscover.err.changed');
    expect(refusalKey('judgement_changed')).toBe('rediscover.err.changed');
    expect(refusalKey('internal')).toBeNull();
    expect(refusalKey(undefined)).toBeNull();
  });
});
