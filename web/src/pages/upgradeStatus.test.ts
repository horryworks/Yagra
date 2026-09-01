// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { UpgradeStatus } from '../types/api';
import type { ComponentRow, Convergence } from './upgradeStatus';
import {
  CORE_ID,
  UPGRADE_PROGRESS_STEPS,
  UPGRADE_RUN_STEPS,
  buildKind,
  bundleTagFromFilename,
  canApply,
  canOffer,
  canUploadBundle,
  componentReason,
  convergePhase,
  convergeProgress,
  convergeState,
  darkPools,
  defaultSelection,
  isRunning,
  lastChecked,
  looksLikeReleaseTag,
  mechanism,
  rollback,
  rollbacks,
  rowLocked,
  rowPlan,
  runPhase,
  runState,
  runStep,
  shortRef,
  shouldPoll,
  shouldPollConvergence,
  stepProgress,
  switchPending,
  unpreparedSites,
  upgrades,
  uptime,
} from './upgradeStatus';

function status(over: Partial<UpgradeStatus> = {}): UpgradeStatus {
  return {
    enabled: false,
    upgrade_enabled: true,
    offers: [],
    updater: {
      // The deployment shape, not the sidecar's health. `true` throughout these fixtures because
      // it is the shape every other case here is about — a deployment that *has* a mechanism,
      // whose sidecar may then be missing, dead, paused or ready.
      installed: true,
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
    components: [],
    poller_convergence: null,
    ...over,
  } as UpgradeStatus;
}

const updater = (present: boolean, fresh: boolean, allowBundle = false, paused = false) => ({
  updater: {
    installed: true,
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
  // The boundary that must not collapse into `absent`. A deployment with no mechanism is a
  // property of how it was installed and nothing on the host will change it; a deployment that has
  // one and has heard nothing is a container to go and look at. Deriving the first from `present`
  // would have made the second disappear from the page that reports on it — the page going quiet
  // at exactly the moment something broke.
  it('separates a deployment with no mechanism from one whose sidecar never reported', () => {
    const none = status({ updater: { ...updater(false, false).updater, installed: false } });
    expect(mechanism(none)).toBe('unsupported');
    expect(mechanism(status(updater(false, false)))).toBe('absent');
  });

  // Not being installed outranks the switch, exactly as it does in `reachable()` on the backend.
  // The switch column defaults to on, so reading it first would tell a deployment that has no
  // updater that its updater is merely switched off.
  it('reports no mechanism regardless of the stored switch', () => {
    for (const upgrade_enabled of [true, false]) {
      const none = status({
        updater: { ...updater(true, true).updater, installed: false },
        upgrade_enabled,
      });
      expect(mechanism(none)).toBe('unsupported');
    }
  });

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

describe('components', () => {
  const row = (over: Partial<ComponentRow>): ComponentRow =>
    ({
      id: 'edge-1',
      kind: 'poller',
      pool: 'default',
      version: '0.2.0',
      upgradable: true,
      reason: null,
      co_located: false,
      moves_back: false,
      live_in_pool: 2,
      needs_site_prep: false,
      progress: null,
      ...over,
    }) as ComponentRow;

  const core = row({ id: CORE_ID, kind: 'core', pool: null, version: '0.2.1', live_in_pool: 0 });

  // 🚨 The accepting case is asserted first and is the load-bearing one. A `rowPlan` that answered
  // `blocked` for everything would satisfy every exclusion below while making the dialog empty and
  // the button dead -- which looks exactly like a deployment with nothing to do.
  it('says what each row would do, and the `v` is a tag convention rather than a version', () => {
    expect(rowPlan(row({ version: '0.2.0' }), '0.2.1')).toBe('moves');
    expect(rowPlan(row({ version: '0.2.1' }), '0.2.1')).toBe('already');
    expect(rowPlan(row({ version: 'v0.2.1' }), '0.2.1')).toBe('already');
    expect(rowPlan(row({ version: '0.2.1' }), 'v0.2.1')).toBe('already');
    expect(rowPlan(row({ upgradable: false }), '0.2.1')).toBe('blocked');
  });

  // The two reasons a checkbox is closed read differently on screen and must not be conflated: a
  // co-located poller *does* move (with core), it just has no box of its own; a blocked row does
  // not move at all.
  it('locks a co-located row and a blocked row, and leaves an ordinary one open', () => {
    expect(rowLocked(row({}), '0.2.1')).toBe(false);
    expect(rowLocked(row({ co_located: true }), '0.2.1')).toBe(true);
    expect(rowLocked(row({ upgradable: false }), '0.2.1')).toBe(true);
    expect(rowLocked(row({ version: '0.2.1' }), '0.2.1')).toBe(true);
  });

  // The dialog opens with everything that would move ticked. What must NOT be in it is anything
  // whose box is closed: the footer counts this list, so a locked row in it would promise work the
  // request cannot ask for.
  it('opens with every movable row selected, and nothing else', () => {
    const rows = [
      core,
      row({ id: 'moves' }),
      row({ id: 'co-located', co_located: true }),
      row({ id: 'blocked', upgradable: false }),
      row({ id: 'already', version: '0.2.1' }),
    ];
    // Core is on 0.2.1 here, so a newer target moves everything that can move.
    expect(defaultSelection(rows, '0.2.2')).toEqual([CORE_ID, 'moves', 'already']);
    // 🚨 And the target core is *already on* excludes core — which is the poller-only entrance,
    // reached from the running release's own row. Getting this wrong would send a request core
    // refuses (`core_not_on_target`), or worse, restart a deployment nobody asked to restart.
    expect(defaultSelection(rows, '0.2.1')).toEqual(['moves']);
    expect(defaultSelection(rows, '0.2.0')).toEqual([CORE_ID, 'already']);
  });

  // 🚨 The warning follows the checkboxes, which is the whole reason it stopped being a field on
  // the response. Computed once from everything that could move, it reads exactly the same when it
  // matters as when it does not.
  it('names the pools that go dark for this selection, not for the whole fleet', () => {
    const rows = [
      core,
      row({ id: 'lone', pool: 'tokyo', live_in_pool: 1 }),
      row({ id: 'paired-a', pool: 'osaka', live_in_pool: 2 }),
      row({ id: 'paired-b', pool: 'osaka', live_in_pool: 2 }),
    ];
    expect(darkPools(rows, ['lone', 'paired-a', 'paired-b'])).toEqual(['tokyo']);
    expect(darkPools(rows, ['paired-a'])).toEqual([]);
    expect(darkPools(rows, [])).toEqual([]);
    // core has no pool, so selecting it can never darken one.
    expect(darkPools(rows, [CORE_ID])).toEqual([]);
  });

  // 🚨 The sibling of the one above, and it follows the checkboxes for the same reason: a site
  // unticked is a site this press does not touch, so leaving it in the sentence would train the
  // operator to read past the sentence.
  //
  // The accepting case is asserted first and is load-bearing. `unpreparedSites` returning nothing
  // is what the screen looked like before this existed -- and the failure it guards against is a
  // remote site destroyed by one click, reported as a success by both ends.
  it('names the sites in this selection that have not said an upgrade is safe there', () => {
    const rows = [
      core,
      row({ id: 'unprepared-b', needs_site_prep: true }),
      row({ id: 'unprepared-a', needs_site_prep: true }),
      row({ id: 'prepared', needs_site_prep: false }),
    ];
    expect(unpreparedSites(rows, ['unprepared-a', 'unprepared-b', 'prepared'])).toEqual([
      'unprepared-a',
      'unprepared-b',
    ]);
    expect(unpreparedSites(rows, ['unprepared-a', 'prepared'])).toEqual(['unprepared-a']);
    expect(unpreparedSites(rows, ['prepared'])).toEqual([]);
    expect(unpreparedSites(rows, [])).toEqual([]);
    // core is never a site, so selecting it cannot put one in the warning.
    expect(unpreparedSites(rows, [CORE_ID])).toEqual([]);
  });

  // A value from a core newer than this bundle must render as *something*. Not manners: the Tier1
  // mocks are generated from the OpenAPI document and fill enums with whatever the generator
  // produces, so a reader that trusts the string paints a raw `t()` key and the walk stays green.
  it('reads a reason and a converge state defensively', () => {
    expect(componentReason('offline')).toBe('offline');
    expect(componentReason('teleported')).toBeNull();
    expect(componentReason(null)).toBeNull();
    expect(convergeState('applying')).toBe('applying');
    expect(convergeState('ymock-0')).toBeNull();
  });
});

describe('convergePhase', () => {
  const conv = (over: Partial<Convergence> = {}): Convergence =>
    ({
      run_id: 'run-1',
      tag: 'v0.2.2',
      requested_by: 'horry',
      started_at: 1,
      finished_at: null,
      targets: [
        { id: 'a', pool: 'default', state: 'returned' },
        { id: 'b', pool: 'default', state: 'applying' },
      ],
      ...over,
    }) as Convergence;

  // The window the server cannot answer in: the convergence is started by a spawned task, so for a
  // moment after the 202 the snapshot is absent or still the previous one. Reading the server alone
  // there renders "nothing is happening" over a press that was accepted.
  it('separates asked-and-not-visible-yet from idle', () => {
    expect(convergePhase(status(), null).kind).toBe('idle');
    expect(convergePhase(status(), 'run-1').kind).toBe('starting');
  });

  it('counts what has returned and what has not, while it runs', () => {
    const p = convergePhase(status({ poller_convergence: conv() } as Partial<UpgradeStatus>), null);
    expect(p.kind).toBe('running');
    if (p.kind !== 'running') return;
    expect(p.done).toBe(1);
    expect(p.total).toBe(2);
    expect(convergeProgress(p)).toBe(0.5);
  });

  // Failed and skipped both count as finished-with-nothing-more-coming, or the bar would sit short
  // of the end for ever on a pool that stopped.
  it('finishes the bar when a pool stops rather than leaving it short', () => {
    const p = convergePhase(
      status({
        poller_convergence: conv({
          finished_at: 9,
          targets: [
            { id: 'a', pool: 'default', state: 'returned' },
            { id: 'b', pool: 'default', state: 'failed' },
            { id: 'c', pool: 'default', state: 'skipped' },
          ],
        }),
      } as Partial<UpgradeStatus>),
      null,
    );
    expect(p.kind).toBe('done');
    if (p.kind !== 'done') return;
    expect(p.done).toBe(1);
    expect(p.failed).toBe(2);
    expect(convergeProgress(p)).toBe(1);
  });

  // 🚨 A finished convergence is kept, so the page must not report the *previous* one as the
  // outcome of the press just made. Matched by id, exactly as the core run's `pending` is.
  it('does not report an older convergence as the answer to this press', () => {
    const st = status({
      poller_convergence: conv({ finished_at: 9 }),
    } as Partial<UpgradeStatus>);
    expect(convergePhase(st, null).kind).toBe('done');
    expect(convergePhase(st, 'run-2').kind).toBe('starting');
    expect(convergePhase(st, 'run-1').kind).toBe('done');
  });

  // The beat is armed by what the SERVER says, not only by what this browser did -- which is the
  // point of the snapshot living in core. A second tab watches the same convergence.
  it('keeps polling a convergence this browser did not start', () => {
    expect(shouldPollConvergence(status({ poller_convergence: conv() } as Partial<UpgradeStatus>), null)).toBe(
      true,
    );
    expect(
      shouldPollConvergence(
        status({ poller_convergence: conv({ finished_at: 9 }) } as Partial<UpgradeStatus>),
        null,
      ),
    ).toBe(false);
    expect(shouldPollConvergence(status(), null)).toBe(false);
  });
});

describe('uptime', () => {
  it('drops to the two largest useful units at each scale', () => {
    expect(uptime(0)).toBe('0m');
    expect(uptime(59)).toBe('0m');
    expect(uptime(60)).toBe('1m');
    expect(uptime(3600)).toBe('1h 0m');
    expect(uptime(3660)).toBe('1h 1m');
    expect(uptime(86_400)).toBe('1d 0h');
    expect(uptime(90_000)).toBe('1d 1h');
  });

  it('never reports minutes once it is reporting days', () => {
    // A process up for weeks is read for "is this the restart I just did"; minutes are noise there.
    expect(uptime(1_000_000)).toBe('11d 13h');
  });
});
