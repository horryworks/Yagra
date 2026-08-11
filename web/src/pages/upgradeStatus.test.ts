// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { UpgradeStatus } from '../types/api';
import {
  buildKind,
  bundleTagFromFilename,
  canApply,
  canOffer,
  canUploadBundle,
  isRunning,
  lastChecked,
  looksLikeReleaseTag,
  mechanism,
  rollback,
  rollbacks,
  runPhase,
  runState,
  runStep,
  shortRef,
  shouldPoll,
  stepProgress,
  switchPending,
  UPGRADE_PROGRESS_STEPS,
  UPGRADE_RUN_STEPS,
  upgrades,
} from './upgradeStatus';

function status(over: Partial<UpgradeStatus> = {}): UpgradeStatus {
  return {
    enabled: false,
    upgrade_enabled: true,
    offers: [],
    updater: {
      present: false,
      fresh: false,
      repo: null,
      last_seen: null,
      check_interval_secs: null,
      allow_bundle: false,
      bundle_max_bytes: null,
      paused: false,
    },
    available: null,
    last_run: null,
    current: {
      core_version: '0.2.1',
      source_ref: 'abcdef1234567890',
      build_profile: 'release',
      hostname: 'core-1',
      uptime_seconds: 42,
    },
    schema: { applied_count: 78, latest_version: 78, compat: null },
    ...over,
  } as UpgradeStatus;
}

const updater = (present: boolean, fresh: boolean, allowBundle = false, paused = false) => ({
  updater: {
    present,
    fresh,
    repo: 'ghcr.io/horryworks',
    last_seen: 1,
    check_interval_secs: 3600,
    allow_bundle: allowBundle,
    bundle_max_bytes: allowBundle ? 4 * 1024 * 1024 * 1024 : null,
    paused,
  },
});

describe('mechanism', () => {
  // The distinction this test exists for: never deployed vs deployed-and-dead are different
  // problems with different fixes, and one of them is a fault that must not look like a setting.
  it('separates never-enabled from enabled-but-stopped', () => {
    expect(mechanism(status(updater(false, false)))).toBe('absent');
    expect(mechanism(status(updater(true, false)))).toBe('stopped');
    expect(mechanism(status(updater(true, true)))).toBe('ready');
  });

  // A choice and a fault must never render the same. `paused` is the operator's switch; `stopped`
  // is the sidecar having died with the switch still on.
  it('separates switched-off from died', () => {
    const off = status({ ...updater(true, true, false, true), upgrade_enabled: false });
    expect(mechanism(off)).toBe('paused');
    const dead = status({ ...updater(true, false), upgrade_enabled: true });
    expect(mechanism(dead)).toBe('stopped');
  });

  // The stored value leads, so a click does not appear to bounce back while the sidecar catches up.
  it('follows the stored switch rather than what the sidecar last saw', () => {
    const justSwitchedOff = status({
      ...updater(true, true, false, false), // the sidecar still reports running
      upgrade_enabled: false, // …but the operator has switched it off
    });
    expect(mechanism(justSwitchedOff)).toBe('paused');
    expect(switchPending(justSwitchedOff)).toBe(true);

    const settled = status({ ...updater(true, true, false, true), upgrade_enabled: false });
    expect(switchPending(settled)).toBe(false);
  });

  // "The mechanism is off" and "you are on the newest version" are different answers, and the page
  // must never render one while meaning the other.
  it('is independent of anything about versions', () => {
    const noFloor = status({
      ...updater(false, false),
      schema: { applied_count: 78, latest_version: 78, compat: null },
    });
    expect(mechanism(noFloor)).toBe('absent');
    expect(rollback(noFloor).kind).toBe('unrestricted');
  });
});

describe('applying', () => {
  it('offers apply only when the updater is alive and nothing is running', () => {
    expect(canApply(status({ enabled: true, ...updater(true, true) }))).toBe(true);
    expect(canApply(status({ enabled: false, ...updater(true, false) }))).toBe(false);
    const busy = status({
      enabled: true,
      ...updater(true, true),
      last_run: { id: 'r', command: 'apply', state: 'running', started_at: 1 },
    });
    expect(isRunning(busy)).toBe(true);
    expect(canApply(busy)).toBe(false);
  });

  it('splits what the backend offered into forwards and backwards', () => {
    const s = status({
      enabled: true,
      ...updater(true, true),
      offers: [
        { tag: 'v0.2.2', core_digest: null, direction: 'upgrade', blocked: null },
        { tag: 'v0.2.0', core_digest: null, direction: 'rollback', blocked: null },
        { tag: 'v0.1.6', core_digest: null, direction: 'rollback', blocked: 'below_floor' },
      ],
    });
    expect(upgrades(s).map((o) => o.tag)).toEqual(['v0.2.2']);
    expect(rollbacks(s).map((o) => o.tag)).toEqual(['v0.2.0', 'v0.1.6']);
  });

  // The bug this guards: with no floor declared the page offered twenty rollbacks, every one of
  // them a binary that cannot start against the applied schema. A blocked offer stays visible —
  // an operator looking for that version needs to see why, not to find it missing — but the
  // button must be dead.
  it('shows a blocked rollback and refuses to let it be pressed', () => {
    const s = status({
      enabled: true,
      ...updater(true, true),
      offers: [
        { tag: 'v0.2.0', core_digest: null, direction: 'rollback', blocked: null },
        { tag: 'v0.1.6', core_digest: null, direction: 'rollback', blocked: 'below_floor' },
      ],
    });
    const [ok, blocked] = rollbacks(s);
    expect(canOffer(s, ok)).toBe(true);
    expect(canOffer(s, blocked)).toBe(false);
  });

  it('offers nothing when the updater never reached a registry', () => {
    const s = status({ available: { written_at: 1, releases: [], error: 'no registry' } });
    expect(upgrades(s)).toEqual([]);
    expect(rollbacks(s)).toEqual([]);
  });

  // A newer updater may invent a state. Rendering a raw key at an operator is the failure mode.
  it('reads an unknown run state as unknown rather than as a translation key', () => {
    expect(runState('succeeded')).toBe('succeeded');
    expect(runState('quiesced')).toBeNull();
    expect(runState(undefined)).toBeNull();
  });
});

describe('offline bundle', () => {
  // A working updater is not consent to install an arbitrary archive: `docker load` takes whatever
  // the file holds, so the deployment opts into that separately.
  it('needs the updater to say it accepts archives, not merely to be alive', () => {
    expect(canUploadBundle(status({ enabled: true, ...updater(true, true, true) }))).toBe(true);
    expect(canUploadBundle(status({ enabled: true, ...updater(true, true, false) }))).toBe(false);
    expect(canUploadBundle(status({ enabled: false, ...updater(true, false, true) }))).toBe(false);
  });

  it('does not offer an upload while one is already running', () => {
    const busy = status({
      enabled: true,
      ...updater(true, true, true),
      last_run: { id: 'r', command: 'bundle', state: 'running', started_at: 1 },
    });
    expect(canUploadBundle(busy)).toBe(false);
  });

  // The property that makes this a pre-filter and not a second copy of the backend's grammar:
  // it may never reject something the backend would take. The list mirrors the Rust test
  // `a_release_tag_is_accepted_in_the_forms_this_project_actually_publishes`.
  it('never rejects a tag the backend accepts', () => {
    for (const ok of ['v0.2.1', 'v1.0.0', 'v0.2.10', 'v0.3.0-beta1', 'v1.2.3-rc2']) {
      expect(looksLikeReleaseTag(ok)).toBe(true);
      expect(looksLikeReleaseTag(`  ${ok}  `)).toBe(true);
    }
  });

  it('catches the mistakes worth catching before a gigabyte is uploaded', () => {
    for (const bad of ['', '   ', '0.2.1', 'latest', 'v0.2.1 --privileged', 'ghcr.io/x:v0.2.1']) {
      expect(looksLikeReleaseTag(bad)).toBe(false);
    }
  });

  it('pre-fills the tag from the archive filename, and gives up quietly', () => {
    expect(bundleTagFromFilename('yagra-v0.2.2.tar')).toBe('v0.2.2');
    expect(bundleTagFromFilename('/downloads/yagra_v1.2.3-rc2_images.tar')).toBe('v1.2.3-rc2');
    expect(bundleTagFromFilename('images.tar')).toBeNull();
  });
});

describe('rollback', () => {
  it('treats no declared floor as reversible', () => {
    expect(rollback(status()).kind).toBe('unrestricted');
  });

  it('surfaces the floor, its reason and the migration that imposed it', () => {
    const r = rollback(
      status({
        schema: {
          applied_count: 91,
          latest_version: 91,
          compat: { min_core: '0.4.0', reason: 'dropped nodes.legacy_addr', since_version: 90 },
        },
      }),
    );
    expect(r).toEqual({
      kind: 'floored',
      minCore: '0.4.0',
      reason: 'dropped nodes.legacy_addr',
      sinceVersion: 90,
    });
  });

  // `compat` is absent rather than null when the backend omits it; both mean the same thing.
  it('reads an omitted floor the same as an explicit null', () => {
    const omitted = status({ schema: { applied_count: 78, latest_version: 78 } });
    expect(rollback(omitted).kind).toBe('unrestricted');
  });
});

describe('buildKind', () => {
  it('separates a release build from a flash build of the same commit', () => {
    expect(buildKind('release')).toBe('release');
    expect(buildKind('ci-fast')).toBe('development');
  });

  it('says unknown rather than guessing when the marker is absent', () => {
    expect(buildKind(null)).toBe('unknown');
    expect(buildKind(undefined)).toBe('unknown');
    expect(buildKind('  ')).toBe('unknown');
  });
});

describe('shortRef', () => {
  it('truncates a long ref and leaves a short one alone', () => {
    expect(shortRef('abcdef1234567890')).toBe('abcdef123456');
    expect(shortRef('abc123')).toBe('abc123');
  });

  it('returns null for an absent or blank ref', () => {
    expect(shortRef(null)).toBeNull();
    expect(shortRef(undefined)).toBeNull();
    expect(shortRef('   ')).toBeNull();
  });
});

describe('runPhase', () => {
  const ran = (over: Record<string, unknown>) =>
    status({
      enabled: true,
      last_run: { id: 'r1', command: 'apply', started_at: 1, ...over },
    } as Partial<UpgradeStatus>);

  it('is idle when nothing was asked for and nothing is running', () => {
    expect(runPhase(status(), null).kind).toBe('idle');
    expect(runPhase(ran({ state: 'succeeded' }), null).kind).toBe('idle');
  });

  // THE regression. core hands the request to the sidecar, which looks for it once every five
  // seconds and then has to start a container, so for 5-10s `status.json` still describes the
  // PREVIOUS run. Reading the server alone there says "succeeded" -- and the page, which armed its
  // poll from exactly that, stopped fetching and sat silent through the whole upgrade.
  it('is starting - not idle, not done - while the sidecar has yet to pick the request up', () => {
    expect(runPhase(status(), 'r2').kind).toBe('starting');
    expect(runPhase(ran({ state: 'succeeded' }), 'r2').kind).toBe('starting');
  });

  it('reports the phase and its position once the sidecar is writing', () => {
    const phase = runPhase(ran({ state: 'running', step: 'pull' }), 'r1');
    expect(phase).toEqual({ kind: 'running', step: 'pull', index: 2, total: 5 });
  });

  it('is running even for a run this browser did not start', () => {
    // Someone else pressed the button, or this tab was reloaded mid-run.
    expect(runPhase(ran({ state: 'running', step: 'backup' }), null).kind).toBe('running');
  });

  it('is done only for the run that was asked for, matched by id', () => {
    expect(runPhase(ran({ state: 'succeeded' }), 'r1')).toEqual({ kind: 'done', state: 'succeeded' });
    expect(runPhase(ran({ state: 'failed' }), 'r1')).toEqual({ kind: 'done', state: 'failed' });
    // A stale pending id must not read an unrelated run as this one's outcome.
    expect(runPhase(ran({ state: 'succeeded' }), 'other').kind).toBe('starting');
  });

  it('survives a step the sidecar knows and this build does not', () => {
    const phase = runPhase(ran({ state: 'running', step: 'quiescing' }), 'r1');
    expect(phase).toMatchObject({ kind: 'running', step: null, index: -1 });
    // No position rather than a position of zero: "unknown phase" is not "nothing has happened".
    expect(stepProgress(phase)).toBeNull();
  });

  it('polls from the act of starting, not from what the server currently says', () => {
    expect(shouldPoll(status(), 'r2')).toBe(true);
    expect(shouldPoll(ran({ state: 'running' }), null)).toBe(true);
    expect(shouldPoll(ran({ state: 'succeeded' }), 'r1')).toBe(false);
    expect(shouldPoll(status(), null)).toBe(false);
  });

  it('refuses a second release during the window where the backend 409 cannot yet see one', () => {
    // Both the UI guard and the server's conflict check read the same status.json, so in this
    // window neither of them knows a run exists. `pending` is the only thing that does.
    const st = status({ enabled: true, last_run: null } as Partial<UpgradeStatus>);
    expect(canApply(st, null)).toBe(true);
    expect(canApply(st, 'r2')).toBe(false);
    expect(canOffer(st, { tag: 'v0.2.2', direction: 'upgrade', blocked: null } as never, 'r2')).toBe(
      false,
    );
    expect(canUploadBundle(status({ enabled: true, ...updater(true, true, true) }), 'r2')).toBe(
      false,
    );
  });
});

describe('run steps', () => {
  it('puts the refusal stamp outside the progress track', () => {
    // `validate` is what a REFUSED request is stamped with, so it needs a label but must never
    // place a bar at 20%.
    expect(UPGRADE_RUN_STEPS).toContain('validate');
    expect(UPGRADE_PROGRESS_STEPS).not.toContain('validate');
    for (const s of UPGRADE_PROGRESS_STEPS) expect(UPGRADE_RUN_STEPS).toContain(s);
  });

  it('narrows an unknown step to null instead of rendering it raw', () => {
    expect(runStep('pull')).toBe('pull');
    expect(runStep('quiescing')).toBeNull();
    expect(runStep(null)).toBeNull();
    expect(runStep(undefined)).toBeNull();
  });

  it('advances monotonically and never reaches 1 before the run ends', () => {
    const at = (step: string) =>
      stepProgress(
        runPhase(
          status({
            enabled: true,
            last_run: { id: 'r1', command: 'apply', state: 'running', step, started_at: 1 },
          } as Partial<UpgradeStatus>),
          'r1',
        ),
      );
    const seen = UPGRADE_PROGRESS_STEPS.map((s) => at(s) ?? 0);
    for (let i = 1; i < seen.length; i += 1) expect(seen[i]).toBeGreaterThan(seen[i - 1]);
    expect(seen[seen.length - 1]).toBeLessThan(1);
    expect(stepProgress({ kind: 'done', state: 'succeeded' })).toBe(1);
  });
});

describe('lastChecked', () => {
  // Not `updater.last_seen`: that is the 5-second heartbeat and is always fresh, so following it
  // would present an 18-hour-old release list as up to date -- which is exactly what happened on
  // the test server the day v0.2.2 shipped.
  it('reads when the registry was read, not when the sidecar last breathed', () => {
    expect(lastChecked(status())).toBeNull();
    const st = status({
      ...updater(true, true),
      available: { written_at: 1_786_400_000, releases: [], error: null },
    } as Partial<UpgradeStatus>);
    expect(lastChecked(st)).toBe(1_786_400_000);
    expect(lastChecked(st)).not.toBe(st.updater.last_seen);
  });
});
