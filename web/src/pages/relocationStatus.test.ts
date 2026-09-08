// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { RelocationStatus } from '../types/api';
import {
  RELOCATION_PROGRESS_STAGES,
  RELOCATION_STAGES,
  canStart,
  knownBytes,
  readiness,
  relocationStage,
  relocationState,
  roomForArchive,
  runPhase,
  shouldPoll,
  stageProgress,
  targetCommands,
  validateTarget,
} from './relocationStatus';
import type { TargetForm } from './relocationStatus';

function status(over: Partial<RelocationStatus> = {}): RelocationStatus {
  return {
    supported: true,
    enabled: true,
    upgrade_enabled: true,
    updater: {
      installed: true,
      present: true,
      fresh: true,
      repo: 'ghcr.io/horryworks',
      last_seen: 1,
      check_interval_secs: 86400,
      allow_bundle: false,
      bundle_max_bytes: null,
      paused: false,
    },
    run: null,
    archive: null,
    free_bytes: 100,
    estimate_bytes: 10,
    needed_bytes: 20,
    ...over,
  } as RelocationStatus;
}

function run(over: Record<string, unknown> = {}) {
  return {
    id: 'run-1',
    mode: 'push',
    state: 'running',
    stage: 'backup',
    message: null,
    target_host: '192.0.2.10',
    host_key_fingerprint: null,
    docker_installed: false,
    target_url: null,
    filename: null,
    size_bytes: null,
    started_at: 1,
    finished_at: null,
    requested_by: 'admin',
    metrics: true,
    images: false,
    tier2: true,
    ...over,
  } as RelocationStatus['run'];
}

describe('readiness', () => {
  it('narrows in the order an operator acts on', () => {
    expect(readiness(status())).toBe('ready');
    expect(readiness(status({ supported: false }))).toBe('updater_too_old');
    expect(readiness(status({ upgrade_enabled: false }))).toBe('paused');
    expect(
      readiness(status({ updater: { ...status().updater, fresh: false } })),
    ).toBe('stopped');
    expect(
      readiness(status({ updater: { ...status().updater, present: false } })),
    ).toBe('absent');
    expect(
      readiness(status({ updater: { ...status().updater, installed: false } })),
    ).toBe('unsupported');
  });

  it('reports a dead sidecar as dead, never as out of date', () => {
    // The ordering that matters: a sidecar that is not running declares no capability either, so
    // asking about the capability first would send the operator to the upgrade page for a
    // container that has simply stopped.
    const dead = status({
      supported: false,
      updater: { ...status().updater, fresh: false },
    });
    expect(readiness(dead)).toBe('stopped');
  });
});

describe('defensive token reads', () => {
  it('keeps what it knows and drops what it does not', () => {
    expect(relocationState('running')).toBe('running');
    expect(relocationState('teleporting')).toBeNull();
    expect(relocationState(undefined)).toBeNull();
    expect(relocationStage('tier2')).toBe('tier2');
    expect(relocationStage('validate')).toBe('validate');
    expect(relocationStage('quantum')).toBeNull();
  });

  it('keeps the refusal stamp off the progress track', () => {
    expect(RELOCATION_STAGES).toContain('validate');
    expect([...RELOCATION_PROGRESS_STAGES]).not.toContain('validate');
  });
});

describe('runPhase', () => {
  it('is idle with nothing running and nothing pending', () => {
    expect(runPhase(status(), null)).toEqual({ kind: 'idle' });
  });

  it('is starting in the window before the sidecar has written anything', () => {
    // The failure this exists for: the server still describes the *previous* run for the first
    // 5-10 seconds, so reading it alone shows someone else's outcome as this button's answer.
    expect(runPhase(status(), 'run-2')).toEqual({ kind: 'starting' });
    const older = status({ run: run({ id: 'run-1', state: 'done', stage: 'archive' }) });
    expect(runPhase(older, 'run-2')).toEqual({ kind: 'starting' });
  });

  it('treats requested and running alike', () => {
    for (const state of ['requested', 'running']) {
      const phase = runPhase(status({ run: run({ state }) }), 'run-1');
      expect(phase.kind).toBe('running');
    }
  });

  it('reports the outcome of the run this browser started', () => {
    const done = status({ run: run({ state: 'done', stage: 'push', finished_at: 9 }) });
    const phase = runPhase(done, 'run-1');
    expect(phase).toMatchObject({ kind: 'done', state: 'done' });
  });

  it('still shows a finished run nobody here started', () => {
    // History is worth showing: a second tab, or the same tab after a reload, has no `pending`.
    const done = status({ run: run({ state: 'failed', stage: 'preflight' }) });
    expect(runPhase(done, null)).toMatchObject({ kind: 'done', state: 'failed' });
  });

  it('falls back to failed rather than rendering an unknown state', () => {
    const weird = status({ run: run({ state: 'exploded' }) });
    expect(runPhase(weird, 'run-1')).toMatchObject({ kind: 'done', state: 'failed' });
  });
});

describe('shouldPoll', () => {
  it('is armed by the act of starting, not by what the server says', () => {
    expect(shouldPoll(status(), null)).toBe(false);
    expect(shouldPoll(status(), 'run-2')).toBe(true);
    expect(shouldPoll(status({ run: run() }), null)).toBe(true);
    expect(shouldPoll(status({ run: run({ state: 'done' }) }), null)).toBe(false);
  });
});

describe('stageProgress', () => {
  it('is null when there is no determinate position', () => {
    expect(stageProgress({ kind: 'idle' })).toBeNull();
    expect(stageProgress({ kind: 'starting' })).toBeNull();
    expect(
      stageProgress({ kind: 'running', stage: null, index: -1, total: 9 }),
    ).toBeNull();
  });

  it('counts the current stage as reached and finishes at one', () => {
    const phase = runPhase(status({ run: run({ stage: 'start' }) }), 'run-1');
    expect(stageProgress(phase)).toBeCloseTo(1 / 10);
    const done = runPhase(status({ run: run({ state: 'done' }) }), 'run-1');
    expect(stageProgress(done)).toBe(1);
  });
});

describe('canStart', () => {
  it('follows the backend, and closes the window the backend cannot see', () => {
    expect(canStart(status())).toBe(true);
    expect(canStart(status({ enabled: false }))).toBe(false);
    // Accepted, not yet visible to the server — and the server's own 409 reads the same file, so
    // without this the operator can start a second one in that window.
    expect(canStart(status(), 'run-2')).toBe(false);
  });
});

describe('targetCommands', () => {
  it('names the file the operator downloaded', () => {
    const cmds = targetCommands('yagra-relocation-20260908T101530Z.tar.gz');
    expect(cmds).toHaveLength(3);
    expect(cmds[1]).toContain('yagra-relocation-20260908T101530Z.tar.gz');
    expect(cmds[2]).toBe('./yagra-relocate.sh');
  });
});

describe('validateTarget', () => {
  const good: TargetForm = {
    host: '192.0.2.10',
    port: '22',
    user: 'ubuntu',
    dir: 'yagra',
    kind: 'password',
    secret: 'hunter2',
  };

  it('accepts what the backend accepts', () => {
    expect(validateTarget(good)).toEqual([]);
    expect(validateTarget({ ...good, host: 'new-nms.example.com' })).toEqual([]);
    expect(validateTarget({ ...good, host: '2001:db8::1' })).toEqual([]);
    expect(validateTarget({ ...good, user: '_svc-yagra' })).toEqual([]);
  });

  it('names each field that is wrong', () => {
    expect(validateTarget({ ...good, host: '' })).toContain('badHost');
    expect(validateTarget({ ...good, host: 'a host' })).toContain('badHost');
    expect(validateTarget({ ...good, port: '0' })).toContain('badPort');
    expect(validateTarget({ ...good, port: '99999' })).toContain('badPort');
    expect(validateTarget({ ...good, port: 'ssh' })).toContain('badPort');
    expect(validateTarget({ ...good, user: 'Root' })).toContain('badUser');
    expect(validateTarget({ ...good, user: '1st' })).toContain('badUser');
    expect(validateTarget({ ...good, dir: '../etc' })).toContain('badDir');
    expect(validateTarget({ ...good, dir: 'a/b' })).toContain('badDir');
    expect(validateTarget({ ...good, secret: '' })).toContain('noSecret');
  });

  it('reports every problem at once rather than one at a time', () => {
    expect(validateTarget({ host: '', port: 'x', user: 'A', dir: '', kind: 'key', secret: '' }))
      .toHaveLength(5);
  });
});

describe('the size numbers', () => {
  it('treats an unmeasurable value as unknown, not as zero', () => {
    expect(knownBytes(0)).toBe(0);
    expect(knownBytes(null)).toBeNull();
    expect(knownBytes(undefined)).toBeNull();
    expect(knownBytes(Number.NaN)).toBeNull();
  });

  it('says "unknown" rather than guessing when core could not measure', () => {
    expect(roomForArchive(status())).toBe(true);
    expect(roomForArchive(status({ free_bytes: 5 }))).toBe(false);
    expect(roomForArchive(status({ free_bytes: null }))).toBeNull();
    expect(roomForArchive(status({ needed_bytes: null }))).toBeNull();
  });
});
