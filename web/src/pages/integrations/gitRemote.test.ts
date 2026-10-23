// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { PushStatusView } from '../../types/api';
import {
  authForUrl,
  canSaveRemote,
  gitRemoteChip,
  pushLine,
  remoteBody,
  storedTokenUsable,
} from './gitRemote';

const status = (over: Partial<PushStatusView> = {}): PushStatusView => ({
  configured: true,
  host: 'example.com',
  pushed_at: null,
  pushed_commit: null,
  failure: null,
  failed_at: null,
  failures: 0,
  ...over,
});

describe('the last push', () => {
  it('reads nothing set, waiting, pushed and failing', () => {
    expect(pushLine(status({ configured: false })).key).toBe('git.push.none');
    expect(pushLine(status()).key).toBe('git.push.waiting');
    expect(
      pushLine(status({ pushed_at: '2026-10-23T05:20:00Z', pushed_commit: 'a1b2c3d4e5f6' })),
    ).toEqual({ key: 'git.push.ok', at: '2026-10-23T05:20:00Z', head: 'a1b2c3d' });
    // A failure outranks an earlier success.
    expect(
      pushLine(
        status({
          pushed_at: '2026-10-23T05:20:00Z',
          pushed_commit: 'a1b2c3d',
          failure: 'auth',
          failed_at: '2026-10-23T06:00:00Z',
        }),
      ),
    ).toEqual({ key: 'git.push.failed', at: '2026-10-23T06:00:00Z', failure: 'auth' });
  });

  it('gives the catalogue tile a chip for each', () => {
    expect(gitRemoteChip(status({ configured: false })).tone).toBe('idle');
    expect(gitRemoteChip(status()).labelKey).toBe('git.status.waiting');
    expect(gitRemoteChip(status({ pushed_at: 'x', pushed_commit: 'y' })).labelKey).toBe('git.status.ok');
    expect(gitRemoteChip(status({ failure: 'unreachable', failed_at: 'x' })).labelKey).toBe(
      'git.status.failed',
    );
  });
});

describe('the form', () => {
  it('reads how a destination is reached from the shape of its URL', () => {
    expect(authForUrl('https://github.com/acme/c.git')).toBe('https_token');
    expect(authForUrl('ssh://git@bitbucket.org/acme/c.git')).toBe('ssh_key');
    expect(authForUrl(' git@gitlab.com:acme/c.git ')).toBe('ssh_key');
    expect(authForUrl('http://github.com/acme/c.git')).toBeNull();
    expect(authForUrl('/home/oxidized/c.git')).toBeNull();
    expect(authForUrl('')).toBeNull();
  });

  it('sends a token only when one was typed, and never one for a key', () => {
    const form = { url: 'https://example.com/a.git', branch: '', username: ' bot ', token: '' };
    expect(remoteBody(form)).toEqual({
      url: 'https://example.com/a.git',
      branch: null,
      auth: 'https_token',
      username: 'bot',
    });
    expect(remoteBody({ ...form, token: ' t ' })?.token).toBe('t');
    expect(remoteBody({ ...form, url: 'git@example.com:a.git', token: 't' })).toEqual({
      url: 'git@example.com:a.git',
      branch: null,
      auth: 'ssh_key',
    });
    expect(remoteBody({ ...form, url: 'ftp://x' })).toBeNull();
  });

  it('keeps a stored token only for the host and port it was given for', () => {
    const was = 'https://GitHub.com/acme/a.git';
    expect(storedTokenUsable(was, 'https_token', 'https://github.com/acme/b.git')).toBe(true);
    expect(storedTokenUsable(was, 'https_token', 'https://example.com/acme/a.git')).toBe(false);
    expect(storedTokenUsable(was, 'https_token', 'https://github.com:8443/acme/a.git')).toBe(false);
    expect(storedTokenUsable(was, 'ssh_key', 'https://github.com/acme/a.git')).toBe(false);
    expect(storedTokenUsable(null, null, 'https://github.com/acme/a.git')).toBe(false);
  });

  it('can be saved once it says enough', () => {
    const form = { url: 'https://example.com/a.git', branch: '', username: 'bot', token: '' };
    expect(canSaveRemote(form, false)).toBe(false);
    expect(canSaveRemote(form, true)).toBe(true);
    expect(canSaveRemote({ ...form, token: 't' }, false)).toBe(true);
    expect(canSaveRemote({ ...form, username: '' }, true)).toBe(false);
    expect(canSaveRemote({ ...form, url: 'git@example.com:a.git', username: '' }, false)).toBe(true);
    expect(canSaveRemote({ ...form, url: 'nope' }, true)).toBe(false);
  });
});
