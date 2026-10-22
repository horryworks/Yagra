// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { ConfigBackupView } from '../../types/api';
import { bodyOf, formOf, isDirty, isStale, previousOf, resultTone, shortOid } from './configBackupForm';

const view = (over: Partial<ConfigBackupView> = {}): ConfigBackupView => ({
  available: true,
  enabled: false,
  credential_id: null,
  input: 'ssh',
  model_override: null,
  profile_model: 'ios',
  readiness: 'off',
  status: null,
  models: ['ios', 'nxos'],
  ...over,
});

describe('config backup form (ADR-210 stage 2)', () => {
  it('opens as what is stored and sends "none" and "from the profile" as null', () => {
    const form = formOf(view());
    expect(form).toEqual({ enabled: false, credentialId: '', input: 'ssh', model: '' });
    expect(bodyOf(form)).toEqual({
      enabled: false,
      credential_id: null,
      input: 'ssh',
      model_override: null,
    });
    const picked = { ...form, enabled: true, credentialId: 'c1', model: 'nxos' };
    expect(bodyOf(picked).model_override).toBe('nxos');
    expect(bodyOf(picked).credential_id).toBe('c1');
  });

  it('opens a stored unknown input as the default, which is then a change', () => {
    const v = view({ input: 'unknown' });
    expect(formOf(v).input).toBe('ssh');
  });

  it('is dirty only when something differs', () => {
    const v = view({ enabled: true, credential_id: 'c1' });
    expect(isDirty(formOf(v), v)).toBe(false);
    expect(isDirty({ ...formOf(v), input: 'telnet' }, v)).toBe(true);
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

describe('status', () => {
  it('is stale after three missed reads, and an unparseable time is not', () => {
    const now = Date.parse('2026-10-22T10:00:00Z');
    expect(isStale('2026-10-22T09:50:00Z', now)).toBe(false);
    expect(isStale('2026-10-22T09:40:00Z', now)).toBe(true);
    expect(isStale('not a time', now)).toBe(false);
  });

  it('draws success as ok, never as nothing, and every other word as a failure', () => {
    expect(resultTone('success')).toBe('ok');
    expect(resultTone('never')).toBe('none');
    expect(resultTone(null)).toBe('none');
    expect(resultTone('no_connection')).toBe('bad');
    expect(resultTone('timelimit')).toBe('bad');
  });
});
