// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { ConfigBackupRow, SaveRecord } from '../types/api';
import { autoSaveFailed, autoSaveResult, autoSaveTargets, canAutoSave, lastAutoSave } from './configAutoSave';

function row(id: string, over: Partial<ConfigBackupRow>): ConfigBackupRow {
  return {
    node_id: id,
    enabled: true,
    configured: true,
    model: 'vrp',
    input: 'ssh',
    credential_id: null,
    login_confirmed: true,
    pending: false,
    try_after: null,
    last: null,
    last_success_at: null,
    poller: 'ready',
    auto_save: false,
    save_supported: true,
    last_auto_save: null,
    ...over,
  };
}

function save(over: Partial<SaveRecord>): SaveRecord {
  return {
    id: 's',
    origin: 'auto',
    state: 'saved',
    requested_at: '2026-11-19T05:56:00Z',
    finished_at: '2026-11-19T05:56:30Z',
    kept_startup: true,
    ...over,
  };
}

describe('automatic save per node', () => {
  it('can be switched only on a node backed up with a model Yagra can save', () => {
    expect(canAutoSave(row('a', {}))).toBe(true);
    expect(canAutoSave(row('b', { enabled: false }))).toBe(false);
    expect(canAutoSave(row('c', { save_supported: false }))).toBe(false);
  });

  it('switches only the rows that would change', () => {
    const rows = [
      row('on', { auto_save: true }),
      row('off', {}),
      row('stopped', { enabled: false }),
      row('asa', { save_supported: false }),
    ];
    expect(autoSaveTargets(rows, true)).toEqual(['off']);
    expect(autoSaveTargets(rows, false)).toEqual(['on']);
  });

  it('reads the newest automatic save, not a person’s', () => {
    const person = save({ id: 'p', origin: 'person' });
    const auto = save({ id: 'a' });
    expect(lastAutoSave([person, auto])?.id).toBe('a');
    expect(lastAutoSave([person])).toBeNull();
  });

  it('marks a failure only while the node is switched on', () => {
    const failed = save({ state: 'failed', failure: 'denied' });
    expect(autoSaveFailed(row('a', { auto_save: true, last_auto_save: failed }))).toBe(true);
    expect(autoSaveFailed(row('a', { auto_save: false, last_auto_save: failed }))).toBe(false);
    expect(autoSaveFailed(row('a', { auto_save: true, last_auto_save: save({}) }))).toBe(false);
    expect(autoSaveFailed(row('a', { auto_save: true }))).toBe(false);
  });

  it('says how the last automatic save went', () => {
    expect(autoSaveResult(null)).toEqual({ kind: 'none' });
    expect(autoSaveResult(save({}))).toEqual({ kind: 'saved', at: '2026-11-19T05:56:30Z' });
    expect(autoSaveResult(save({ state: 'failed', failure: 'auth', finished_at: undefined }))).toEqual({
      kind: 'failed',
      at: '2026-11-19T05:56:00Z',
      failure: 'auth',
    });
    expect(autoSaveResult(save({ state: 'failed' }))).toMatchObject({ failure: 'other' });
    expect(autoSaveResult(save({ state: 'running' }))).toEqual({ kind: 'busy' });
  });
});
