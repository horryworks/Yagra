// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { UpgradeStatus } from '../types/api';
import {
  buildKind,
  bundleTagFromFilename,
  canApply,
  canUploadBundle,
  isRunning,
  looksLikeReleaseTag,
  mechanism,
  offerableReleases,
  rollback,
  runState,
  shortRef,
} from './upgradeStatus';

function status(over: Partial<UpgradeStatus> = {}): UpgradeStatus {
  return {
    enabled: false,
    updater: {
      present: false,
      fresh: false,
      repo: null,
      last_seen: null,
      check_interval_secs: null,
      allow_bundle: false,
      bundle_max_bytes: null,
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

const updater = (present: boolean, fresh: boolean, allowBundle = false) => ({
  updater: {
    present,
    fresh,
    repo: 'ghcr.io/horryworks',
    last_seen: 1,
    check_interval_secs: 3600,
    allow_bundle: allowBundle,
    bundle_max_bytes: allowBundle ? 4 * 1024 * 1024 * 1024 : null,
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

  it('never offers the version already running as somewhere to move to', () => {
    const s = status({
      available: {
        written_at: 1,
        releases: [{ tag: 'v0.2.2', core_digest: null }, { tag: 'v0.2.1', core_digest: null }],
        error: null,
      },
    });
    expect(offerableReleases(s)).toEqual(['v0.2.2']); // current is 0.2.1
  });

  it('offers nothing when the updater never reached a registry', () => {
    const s = status({ available: { written_at: 1, releases: [], error: 'no registry' } });
    expect(offerableReleases(s)).toEqual([]);
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
