// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { AttemptView, ConfigBackupView, SaveRecord } from '../../types/api';
import {
  attemptLine,
  bodyOf,
  canOfferSave,
  formOf,
  isDirty,
  isQuietFetchRefusal,
  loginLabel,
  orderShown,
  pickedLogins,
  previousOf,
  refreshMs,
  saveLine,
  shortOid,
  stateLine,
} from './configBackupForm';

const view = (over: Partial<ConfigBackupView> = {}): ConfigBackupView => ({
  available: true,
  enabled: false,
  login_order: null,
  candidates: ['a', 'b', 'c'],
  credential_id: null,
  login_confirmed: false,
  pending: false,
  try_after: null,
  exhausted: false,
  attempts: [],
  input: 'ssh',
  scan_mode: 'normal',
  model_override: null,
  profile_model: 'ios',
  readiness: 'off',
  status: null,
  models: ['ios', 'nxos'],
  save_supported: true,
  saves: [],
  ...over,
});

const save = (over: Partial<SaveRecord>): SaveRecord => ({
  id: 's1',
  origin: 'person',
  state: 'saved',
  requested_at: '2026-10-31T10:00:00Z',
  kept_startup: true,
  ...over,
});

describe('config save (ADR-222)', () => {
  const ready = view({ enabled: true, login_confirmed: true, readiness: 'ready', credential_id: 'b' });

  it('offers the button only for a backed-up node with a working login and a savable model', () => {
    expect(canOfferSave(ready)).toBe(true);
    expect(canOfferSave({ ...ready, save_supported: false })).toBe(false);
    expect(canOfferSave({ ...ready, login_confirmed: false })).toBe(false);
    expect(canOfferSave({ ...ready, enabled: false })).toBe(false);
    expect(canOfferSave({ ...ready, readiness: 'no_poller' })).toBe(false);
    expect(canOfferSave({ ...ready, available: false })).toBe(false);
  });

  it('hides the button while a save is waiting or running, and reads again fast meanwhile', () => {
    for (const state of ['pending', 'running'] as const) {
      const v = { ...ready, saves: [save({ state })] };
      expect(canOfferSave(v)).toBe(false);
      expect(refreshMs(v)).toBe(3_000);
    }
    expect(refreshMs({ ...ready, saves: [save({ state: 'saved' })] })).toBeUndefined();
  });

  it('names a failure by its kind and a missing kind as other', () => {
    expect(saveLine(save({ state: 'saved' }))).toEqual({ key: 'config.saves.state.saved', tone: 'ok' });
    expect(saveLine(save({ state: 'failed', failure: 'denied' })).key).toBe(
      'config.saves.failure.denied',
    );
    expect(saveLine(save({ state: 'failed' })).key).toBe('config.saves.failure.other');
    expect(saveLine(save({ state: 'running' })).tone).toBe('busy');
  });
});

const attempt = (over: Partial<AttemptView>): AttemptView => ({
  at: '2026-10-22T10:00:00Z',
  credential_id: 'b',
  outcome: 'success',
  new_version: null,
  ...over,
});

describe('config backup form (ADR-210)', () => {
  it('opens as what is stored and sends the default order and "from the profile" as null', () => {
    const form = formOf(view());
    expect(form).toEqual({ order: null, input: 'ssh', model: '', scanMode: 'normal' });
    expect(bodyOf(form, true)).toEqual({
      enabled: true,
      login_order: null,
      input: 'ssh',
      model_override: null,
      scan_mode: 'normal',
    });
    const picked = { ...form, order: ['c', 'a'], model: 'nxos', scanMode: 'rapid' as const };
    expect(bodyOf(picked, false)).toEqual({
      enabled: false,
      login_order: ['c', 'a'],
      input: 'ssh',
      model_override: 'nxos',
      scan_mode: 'rapid',
    });
  });

  it('opens a stored unknown input as the default, which is then a change', () => {
    const v = view({ input: 'unknown' });
    expect(formOf(v).input).toBe('ssh');
  });

  it('opens a mode it does not know as the most careful one', () => {
    expect(formOf(view({ scan_mode: 'unknown' })).scanMode).toBe('slow');
    const v = view({ enabled: true, scan_mode: 'slow' });
    expect(isDirty({ ...formOf(v), scanMode: 'rapid' }, v)).toBe(true);
  });

  it('is dirty only when something differs, the order included', () => {
    const v = view({ enabled: true, login_order: ['a', 'b'] });
    expect(isDirty(formOf(v), v)).toBe(false);
    expect(isDirty({ ...formOf(v), input: 'telnet' }, v)).toBe(true);
    expect(isDirty({ ...formOf(v), order: ['b', 'a'] }, v)).toBe(true);
    expect(isDirty({ ...formOf(v), order: null }, v)).toBe(true);
  });

  it('ticks every candidate until the node has an order of its own', () => {
    const v = view();
    expect(pickedLogins(formOf(v), v)).toEqual(['a', 'b', 'c']);
    expect(pickedLogins({ ...formOf(v), order: ['c'] }, v)).toEqual(['c']);
  });
});

describe('naming a login', () => {
  it('uses the name when it is known, else its place in the order, else says it is gone', () => {
    const names = new Map([['a', 'ops-a']]);
    expect(loginLabel('a', names, ['a', 'b'])).toEqual({ kind: 'name', name: 'ops-a' });
    expect(loginLabel('b', names, ['a', 'b'])).toEqual({ kind: 'index', index: 2 });
    expect(loginLabel('z', names, ['a', 'b'])).toEqual({ kind: 'gone' });
    expect(loginLabel(null, names, ['a'])).toEqual({ kind: 'gone' });
  });
});

describe('the state line', () => {
  it('says off, then why it is not fetched, before anything else', () => {
    expect(stateLine(view()).key).toBe('config.readiness.off');
    expect(stateLine(view({ enabled: true, readiness: 'no_model' })).key).toBe(
      'config.readiness.no_model',
    );
  });

  it('names the login being tried and its place among the candidates', () => {
    const line = stateLine(
      view({ enabled: true, readiness: 'ready', pending: true, credential_id: 'b' }),
    );
    expect(line).toEqual({
      key: 'config.state.trying',
      login: 'b',
      params: { n: 2, total: 3 },
      tone: 'busy',
    });
    const known = stateLine(
      view({
        enabled: true,
        readiness: 'ready',
        pending: true,
        credential_id: 'b',
        login_confirmed: true,
      }),
    );
    expect(known.key).toBe('config.state.fetching');
  });

  it('says no login worked, else what the last attempt did, else that it waits', () => {
    const on = { enabled: true, readiness: 'ready' as const };
    expect(stateLine(view({ ...on, exhausted: true })).key).toBe('config.state.exhausted');
    // Stopped because the device went on not answering, which is not "no login worked".
    expect(
      stateLine(
        view({ ...on, exhausted: true, attempts: [attempt({ outcome: 'unreachable' })] }),
      ).key,
    ).toBe('config.state.gaveUp');
    expect(stateLine(view({ ...on, attempts: [attempt({ new_version: true })] })).key).toBe(
      'config.result.newVersion',
    );
    expect(stateLine(view(on)).key).toBe('config.state.waiting');
  });
});

describe('an attempt', () => {
  it('tells a new version from no change, and names the login that was refused', () => {
    expect(attemptLine(attempt({ new_version: true })).key).toBe('config.result.newVersion');
    expect(attemptLine(attempt({ new_version: false })).key).toBe('config.result.noChange');
    expect(attemptLine(attempt({})).key).toBe('config.outcome.success');
    expect(attemptLine(attempt({ outcome: 'auth_failed' }))).toEqual({
      key: 'config.outcome.auth_failed',
      login: 'b',
      tone: 'bad',
    });
    // A device that did not answer is not a failed login.
    expect(attemptLine(attempt({ outcome: 'unreachable' })).tone).toBe('none');
  });
});

describe('reading again', () => {
  it('is fast while a fetch is out, slow while a login is looked for, off once settled', () => {
    expect(refreshMs(null)).toBeUndefined();
    expect(refreshMs(view())).toBeUndefined();
    expect(refreshMs(view({ enabled: true, pending: true }))).toBe(3_000);
    expect(refreshMs(view({ enabled: true }))).toBe(30_000);
    expect(refreshMs(view({ enabled: true, login_confirmed: true }))).toBeUndefined();
    expect(refreshMs(view({ enabled: true, exhausted: true }))).toBeUndefined();
  });

  it('treats an already-running fetch as nothing to report, and anything else as an error', () => {
    expect(isQuietFetchRefusal(409, 'config_backup_pending')).toBe(true);
    expect(isQuietFetchRefusal(409, 'config_backup_not_ready')).toBe(true);
    expect(isQuietFetchRefusal(409, 'config_backup_busy')).toBe(false);
    expect(isQuietFetchRefusal(429, 'config_backup_wait')).toBe(false);
    expect(isQuietFetchRefusal(502, 'oxidized_unreachable')).toBe(false);
  });
});

describe('versions', () => {
  const versions = [
    { oid: 'c'.repeat(40), stored_at: null },
    { oid: 'b'.repeat(40), stored_at: null },
    { oid: 'a'.repeat(40), stored_at: null },
  ];
  it('compares a version with the one stored before it, and the oldest with nothing', () => {
    expect(previousOf(versions, 'c'.repeat(40))).toBe('b'.repeat(40));
    expect(previousOf(versions, 'a'.repeat(40))).toBeNull();
    expect(previousOf(versions, 'z')).toBeNull();
    expect(shortOid('abcdef0123456789')).toBe('abcdef01');
  });
});

describe('the try order', () => {
  it('is open while a login is looked for, and folded under the working one once found', () => {
    const base = { login_confirmed: false, credential_id: null } as unknown as ConfigBackupView;
    expect(orderShown(base, false)).toBe(true);
    const found = { ...base, login_confirmed: true, credential_id: 'c1' } as ConfigBackupView;
    expect(orderShown(found, false)).toBe(false);
    expect(orderShown(found, true)).toBe(true);
  });
});
