// SPDX-License-Identifier: AGPL-3.0-only
// Upgrade (Settings ▸ Upgrade, ADR-050/ADR-051). What this deployment is running, what it is made
// of, and how far back it can be taken. Manage-configuration — it reports build provenance rather
// than a health counter.
//
// **One entrance, two steps** (ADR-051 Inc.6). Pressing Upgrade opens the components list — core
// and every poller, with what each runs and a checkbox — and pressing it again starts the work.
// There used to be three entrances, none of which could be told what to move: the release list, a
// separate "align the pollers" card, and the archive upload. The first two are now one dialog.
//
// The privileged updater sidecar is off by default, so the page says so plainly rather than
// rendering an empty version list, which would read as "you are up to date".
//
// The judgement lives in `upgradeStatus.ts` so it can be unit-tested — Vitest never runs a .tsx
// (testing.md). What is left here is layout.

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { Badge } from '../components/ui/Badge';
import { ProgressBar } from '../components/ui/ProgressBar';
import { api, errMsg } from '../services/api';
import { formatTimestamp, relativeTime } from '../lib/format';
import type { UpgradeStatus } from '../types/api';
import type { ComponentRow, ConvergePhase, Offer, RunPhase } from './upgradeStatus';
import {
  buildKind,
  bundleTagFromFilename,
  canOffer,
  canUploadBundle,
  componentReason,
  convergePhase,
  convergeProgress,
  convergeState,
  CORE_ID,
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
  unpreparedSites,
  runPhase,
  runState,
  shortRef,
  shouldPoll,
  shouldPollConvergence,
  stepProgress,
  switchPending,
  UPGRADE_PROGRESS_STEPS,
  upgrades,
} from './upgradeStatus';
import './UpgradePage.css';

/** How often to re-read the status while a run is in flight.
 *
 *  Two seconds, not five: the whole operation is ~65 seconds measured (backup 5s, pull 34s,
 *  recreate 24s), so a five-second beat draws a bar that moves about a dozen times. Core restarts
 *  in the middle, so most of these requests are expected to fail and the page must not treat that
 *  as an error. */
const POLL_RUNNING_MS = 2_000;

/** Give up polling a run that never reports. The sidecar's own verify loop caps at five minutes and
 *  core closes the window at fifteen, so past this there is nothing left to wait for. */
const POLL_CEILING_MS = 15 * 60_000;

/** How long to keep watching for a manual registry check to land before calling it a miss. The
 *  sidecar's request beat is five seconds and two `wget`s follow it. */
const CHECK_TIMEOUT_MS = 45_000;

/** How often to re-read while pollers converge (ADR-051 Inc.4/Inc.6).
 *
 *  Slower than the run poll, and for the opposite reason: nothing here is being recreated
 *  underneath this browser, and the thing being watched moves on the scale of a site pulling an
 *  image over a WAN link. A beat that outpaces it just re-renders the same rows. */
const ALIGN_POLL_MS = 5_000;

/** A poll has to fail for this long during a run before the page says the connection is gone.
 *  One missed request is normal; a run of them is core being recreated. */
const STALE_AFTER_MS = 6_000;

/** One label/value row, mirroring Settings ▸ About so the two read as one family. */
function Row({ label, children, mono }: { label: string; children: React.ReactNode; mono?: boolean }) {
  return (
    <div className="upgrade-row">
      <div className="upgrade-label muted">{label}</div>
      <div className={mono ? 'upgrade-value mono' : 'upgrade-value'}>{children}</div>
    </div>
  );
}

/** A section divider inside a card. The cards were merged from eight to five, so what used to be a
 *  card title has to survive as something — a word an operator has learned to look for must stay
 *  findable after the box around it is gone (ADR-055 R1). */
function Sub({ children, first }: { children: React.ReactNode; first?: boolean }) {
  return <div className={first ? 'upgrade-subhead first' : 'upgrade-subhead'}>{children}</div>;
}

/**
 * What is happening right now, from the moment the button is pressed until the outcome is in.
 *
 * This is the half that was missing. The page used to render nothing at all between the confirm
 * dialog closing and a manual reload — the operator watched their monitoring go down with no sign
 * the machine had heard them. The run takes about 65 seconds on real hardware, and core is
 * destroyed and recreated inside it, so several of those seconds have no backend to ask.
 *
 * Every state below is one the operator actually passes through:
 *   starting  — requested; the sidecar checks for it every five seconds
 *   running   — a phase the sidecar named, with its position on the track
 *   stale     — the requests are failing, which during `compose` is the operation working
 */
function Progress({
  phase,
  stale,
  target,
  t,
}: {
  phase: RunPhase;
  stale: boolean;
  target: string | null;
  t: (k: string, o?: Record<string, unknown>) => string;
}) {
  const pct = stepProgress(phase);
  const step = phase.kind === 'running' ? phase.step : null;
  return (
    <div className="upgrade-progress">
      <p className="upgrade-note">
        {phase.kind === 'starting'
          ? t('run.starting')
          : step
            ? t(`runStep.${step}`)
            : t('run.working')}
        {target ? ` — ${target}` : ''}
      </p>
      <ProgressBar value={pct} label={t('run.inFlight')} />
      {/* The track spelled out, so "pull" is placed rather than merely named: an operator who has
          not read the ADR still learns that a backup came first and a check comes last. */}
      <ol className="upgrade-steps">
        {UPGRADE_PROGRESS_STEPS.map((s, i) => {
          const done = phase.kind === 'done' || (phase.kind === 'running' && i < phase.index);
          const now = phase.kind === 'running' && i === phase.index;
          return (
            <li
              key={s}
              className={now ? 'upgrade-step upgrade-step-now' : 'upgrade-step'}
              aria-current={now ? 'step' : undefined}
            >
              <span className="upgrade-step-mark" aria-hidden="true">
                {done ? '✓' : now ? '▸' : '·'}
              </span>
              <span className={done || now ? undefined : 'muted'}>{t(`runStep.${s}`)}</span>
            </li>
          );
        })}
      </ol>
      {/* Distinguishes "core is being replaced" from "the page has frozen". The two look identical
          without it, and only one of them is the upgrade working (ADR-050 decision 3). */}
      <p className="upgrade-hint muted">
        {stale ? t('run.reconnecting') : t('run.disconnectWarning')}
      </p>
    </div>
  );
}

/**
 * The remote sites, while they are being replaced and after (ADR-051 Inc.6).
 *
 * The other half of the same press, and the half that had no progress at all: core's own track ends
 * at `verify`, and everything after it — a site pulling over whatever link it has, one container
 * recreated at a time per pool — was invisible for as long as half an hour.
 *
 * ⚠️ **More than one row can be `applying` at once.** Pools converge in parallel and only the queue
 * *within* a pool is serial, so a headline naming a single site is wrong on any deployment with two
 * pools. The list is what carries the answer; the sentence above it counts.
 */
function ConvergeProgress({
  phase,
  t,
}: {
  phase: Extract<ConvergePhase, { kind: 'running' | 'done' }>;
  t: (k: string, o?: Record<string, unknown>) => string;
}) {
  const mark: Record<string, string> = {
    waiting: '·',
    prefetching: '·',
    applying: '▸',
    returned: '✓',
    failed: '✗',
    skipped: '·',
  };
  return (
    <div className="upgrade-progress">
      <p className="upgrade-note">
        {t('converge.count', {
          done: phase.done,
          total: phase.total,
          version: phase.conv.tag,
        })}
      </p>
      <ProgressBar value={convergeProgress(phase)} label={t('converge.heading')} />
      <ul className="upgrade-sites">
        {phase.conv.targets.map((s) => {
          const st = convergeState(s.state);
          const cls =
            st === 'returned'
              ? 'upgrade-site upgrade-site-done'
              : st === 'applying'
                ? 'upgrade-site upgrade-site-now'
                : st === 'failed'
                  ? 'upgrade-site upgrade-site-bad'
                  : 'upgrade-site upgrade-site-wait';
          return (
            <li key={s.id} className={cls}>
              <span className="upgrade-site-mark" aria-hidden="true">
                {(st && mark[st]) ?? '·'}
              </span>
              <span className="mono">{s.id}</span>
              <span className="upgrade-site-pool muted">{s.pool}</span>
              <span className="upgrade-site-state">
                {/* A state this build has never heard of still renders as *something*: the value
                    comes from a core that may be newer than this bundle. */}
                {st ? t(`convergeState.${st}`) : s.state}
              </span>
            </li>
          );
        })}
      </ul>
      <p className="upgrade-hint muted">{t('converge.serial')}</p>
    </div>
  );
}

/** Seconds → a coarse "3d 4h" / "12m" string. Coarse on purpose: this answers "did core restart
 *  while nobody was looking?", not "how long exactly". */
function uptime(seconds: number): string {
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

export function UpgradePage() {
  const { t } = useTranslation('settings-upgrade');
  const [status, setStatus] = useState<UpgradeStatus | null>(null);
  const [failed, setFailed] = useState(false);
  // An upgrade changes the deployment, so an operator is refused the whole page. Distinguish that
  // from "the updater is unreachable" — they need different fixes, and telling one as the other
  // sends an operator looking for a broken container (ADR-056).
  const [block, setBlock] = useState<LoadBlock | null>(null);
  // The release the selection dialog is open for, or `null`. It replaces the old yes/no
  // confirmation: the dialog *is* the confirmation, and it is also where what-moves is decided.
  const [picking, setPicking] = useState<string | null>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const [applyError, setApplyError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [bundleFile, setBundleFile] = useState<File | null>(null);
  const [bundleTag, setBundleTag] = useState('');
  const [bundleError, setBundleError] = useState<string | null>(null);
  // `null` when no upload is in flight; 0…1 while one is.
  const [uploaded, setUploaded] = useState<number | null>(null);
  const [switching, setSwitching] = useState(false);
  const [showOlder, setShowOlder] = useState(false);
  const [switchError, setSwitchError] = useState<string | null>(null);
  // The run this browser asked for, held only until the server reports an outcome for it.
  //
  // ⚠️ It never decides the outcome (ADR-050 decision 3) — it decides whether to keep asking. The
  // page cannot read the server alone here: the sidecar looks for the request once every five
  // seconds and then starts a container, so for 5–10s `status.json` still describes the PREVIOUS
  // run. Arming the poll from `isRunning` therefore never armed it at all, and the page sat silent
  // through the entire upgrade. See `runPhase` for the full account.
  const [pending, setPending] = useState<string | null>(null);
  // The same, for the poller half. Separate because the two can be in flight at once — a core
  // upgrade converges the fleet afterwards — and because only one of them destroys this browser's
  // connection.
  const [pendingAlign, setPendingAlign] = useState<string | null>(null);
  // The tag that was asked for, so the progress card can name it before the server can.
  const [requestedTag, setRequestedTag] = useState<string | null>(null);
  // Set when polls start failing during a run: core is being recreated. Distinct from `failed`,
  // which only ever means "we never got a first answer".
  const [stale, setStale] = useState(false);
  const [checking, setChecking] = useState(false);
  const [checkError, setCheckError] = useState<string | null>(null);
  // Sticky: once a run starts, a failed poll means core is restarting — which is the operation
  // working, not an error. Without this the page would flip to "could not read" mid-upgrade.
  const everSeen = useRef(false);
  const lastOk = useRef<number>(Date.now());
  // The manual check runs its own watch rather than riding the run poll, because it is waiting on a
  // different fact (`available.written_at`) and must stop on its own deadline. Held in a ref so
  // leaving the page cancels it.
  const checkWatch = useRef<number | null>(null);
  useEffect(
    () => () => {
      if (checkWatch.current !== null) window.clearInterval(checkWatch.current);
    },
    [],
  );

  const load = useCallback(async () => {
    try {
      const s = await api.getUpgradeStatus();
      everSeen.current = true;
      lastOk.current = Date.now();
      setStatus(s);
      setFailed(false);
      setStale(false);
    } catch (e: unknown) {
      const b = classifyLoadError(e);
      if (b) setBlock(b);
      else if (!everSeen.current) setFailed(true);
      // The page keeps rendering the last snapshot either way — but it now says so. Before this it
      // showed stale data indistinguishable from live data, which during an upgrade is exactly the
      // moment an operator needs to know which one they are looking at.
      else if (Date.now() - lastOk.current > STALE_AFTER_MS) setStale(true);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // Poll from the act of starting, not from what the server currently says — `DiscoveryPage` arms
  // its interval the same way, and the inversion was the bug. `status` is deliberately not in the
  // dependency list beyond the flag: re-creating the interval on every tick would reset it.
  const polling = status ? shouldPoll(status, pending) : false;
  useEffect(() => {
    if (!polling) return undefined;
    const started = Date.now();
    const h = window.setInterval(() => {
      // A run that never reports must not be polled for ever. Dropping `pending` returns the page
      // to whatever the server last said, which is the honest thing to show.
      if (Date.now() - started > POLL_CEILING_MS) setPending(null);
      else void load();
    }, POLL_RUNNING_MS);
    return () => window.clearInterval(h);
  }, [polling, load]);

  // The convergence has its own, slower beat — and, unlike the run poll, it is armed by **what the
  // server says**, not only by what this browser did. That is the whole point of the snapshot
  // moving to the backend (ADR-051 Inc.6): a second tab, or the same tab after a reload, watches
  // the same convergence. There is no ceiling here for the same reason: the server ends it by
  // stamping `finished_at`, so there is nothing to time out on.
  const converging = status ? shouldPollConvergence(status, pendingAlign) : false;
  useEffect(() => {
    if (!converging) return undefined;
    const h = window.setInterval(() => void load(), ALIGN_POLL_MS);
    return () => window.clearInterval(h);
  }, [converging, load]);

  /** Start the upgrade the dialog is showing.
   *
   *  One call for both halves: `include_core` decides whether this deployment restarts, and the
   *  server refuses a poller-only request that names a release core is not on. Which `pending` is
   *  set follows from that — only a core upgrade destroys this browser's connection. */
  const submit = async () => {
    if (picking === null) return;
    const includeCore = selected.includes(CORE_ID);
    setSubmitting(true);
    setApplyError(null);
    try {
      const accepted = await api.applyUpgrade(
        picking,
        includeCore,
        selected.filter((id) => id !== CORE_ID),
      );
      if (includeCore) {
        setPending(accepted.id);
        setRequestedTag(picking);
      } else {
        setPendingAlign(accepted.id);
      }
      setPicking(null);
      await load();
    } catch (e) {
      setApplyError(errMsg(e, t('apply.failed')));
    } finally {
      setSubmitting(false);
    }
  };

  /** Ask the updater to re-read the registry, then watch for the answer to land.
   *
   *  The POST only says the request was handed over; what proves it worked is `available.written_at`
   *  moving. Watching that rather than the 202 is what lets this report "the updater never answered"
   *  instead of a success the operator cannot see. */
  const checkNow = async () => {
    if (!status) return;
    const before = lastChecked(status);
    setChecking(true);
    setCheckError(null);
    try {
      await api.checkUpgrades();
    } catch (e) {
      setCheckError(errMsg(e, t('mechanism.checkFailed')));
      setChecking(false);
      return;
    }
    const deadline = Date.now() + CHECK_TIMEOUT_MS;
    const stop = () => {
      if (checkWatch.current !== null) window.clearInterval(checkWatch.current);
      checkWatch.current = null;
      setChecking(false);
    };
    checkWatch.current = window.setInterval(() => {
      void (async () => {
        try {
          const s = await api.getUpgradeStatus();
          setStatus(s);
          lastOk.current = Date.now();
          const now = lastChecked(s);
          if (now !== null && now !== before) {
            stop();
            return;
          }
        } catch {
          /* keep waiting — the deadline below is what ends this */
        }
        if (Date.now() > deadline) {
          stop();
          setCheckError(t('mechanism.checkTimeout'));
        }
      })();
    }, 2_000);
  };

  const toggle = async (next: boolean) => {
    setSwitching(true);
    setSwitchError(null);
    try {
      await api.setUpgradeEnabled(next);
      // Re-read rather than patch local state: the sidecar's own view arrives on the next poll and
      // the page should show what the deployment says, not what this browser just asked for.
      await load();
    } catch (e) {
      setSwitchError(errMsg(e, t('mechanism.switchFailed')));
    } finally {
      setSwitching(false);
    }
  };

  const pickBundle = (file: File | null) => {
    setBundleFile(file);
    setBundleError(null);
    // Only ever a suggestion — the updater verifies the tag against the images the archive really
    // holds, so a wrong guess fails the run rather than installing the wrong thing.
    const guess = file ? bundleTagFromFilename(file.name) : null;
    if (guess) setBundleTag(guess);
  };

  const upload = async () => {
    if (!bundleFile) return;
    setBundleError(null);
    setUploaded(0);
    try {
      const accepted = await api.uploadUpgradeBundle(bundleFile, bundleTag.trim(), setUploaded);
      // An archive install is a run like any other — same backup, same compose swap, same verify —
      // so it gets the same progress treatment rather than going quiet after the upload bar fills.
      setPending(accepted.id);
      setRequestedTag(bundleTag.trim());
      setBundleFile(null);
      await load();
    } catch (e) {
      setBundleError(errMsg(e, t('bundle.failed')));
    } finally {
      setUploaded(null);
    }
  };

  const header = (
    <PageHeader
      title={t('title')}
      trail={[{ label: t('nav:sections.settings') }, { label: t('title') }]}
      note={t('subtitle')}
    />
  );

  if (block) {
    return (
      <div>
        {header}
        <LoadBlockNotice block={block} unavailable={t('error')} permission="manage_system" />
      </div>
    );
  }

  if (failed) {
    return (
      <div>
        {header}
        <Card>
          <p className="upgrade-note">{t('error')}</p>
        </Card>
      </div>
    );
  }

  if (!status) {
    return (
      <div>
        {header}
        <Card>
          <p className="upgrade-note muted">{t('loading')}</p>
        </Card>
      </div>
    );
  }

  const kind = buildKind(status.current.build_profile);
  const ref = shortRef(status.current.source_ref);
  const back = rollback(status);
  const rows: ComponentRow[] = status.components;
  const state = mechanism(status);
  const last = status.last_run;
  const lastState = runState(last?.state);
  const newer = upgrades(status);
  const older = rollbacks(status);
  const phase = runPhase(status, pending);
  const inFlight = phase.kind === 'starting' || phase.kind === 'running';
  const cphase = convergePhase(status, pendingAlign);
  // Which release is being installed. Prefer the server's answer, but fall back to the tag this
  // browser asked for: during `starting` the server is still describing the *previous* run, so
  // reading `last.target` alone would name the wrong version on the one screen that must not.
  const pendingTarget =
    (last?.id === pending ? last?.target : null) ?? requestedTag ?? last?.target ?? null;
  const checkedAt = lastChecked(status);
  const uploading = uploaded !== null;
  const tagOk = looksLikeReleaseTag(bundleTag);
  const bundleReady =
    !uploading && canUploadBundle(status, pending) && bundleFile !== null && tagOk;
  const maxBytes = status.updater.bundle_max_bytes ?? null;
  const maxGib = maxBytes === null ? null : Math.round((maxBytes / 1024 ** 3) * 10) / 10;
  // The pollers that are off this core's own build. When there are any, the running release gets a
  // row of its own in the list below — that row is the way in when core is current and only the
  // sites have drifted, and it opens the same dialog with core already excluded.
  const behind = rows.filter(
    (r) => r.kind === 'poller' && rowPlan(r, status.current.core_version) === 'moves',
  );

  const open = (tag: string) => {
    setApplyError(null);
    setSelected(defaultSelection(rows, tag));
    setPicking(tag);
  };

  // One row per release. The button says which way it goes — "upgrade to this" on a release older
  // than the running one is simply false, and this page is where an operator decides whether to
  // touch production.
  const row = (offer: Offer) => (
    <li key={offer.tag} className="upgrade-release">
      <span className="mono">{offer.tag}</span>
      <div className="upgrade-release-action">
        {offer.blocked && (
          <span className="upgrade-blocked muted">{t(`offerBlock.${offer.blocked}`)}</span>
        )}
        <Button
          variant={offer.direction === 'upgrade' ? 'primary' : undefined}
          disabled={!canOffer(status, offer, pending)}
          onClick={() => open(offer.tag)}
        >
          {t(`offerAction.${offer.direction}`)}
        </Button>
      </div>
    </li>
  );

  /** One row of the components list, read-only. The checkboxes live in the dialog: before the
   *  press this is a statement of fact, and only after it is a choice. */
  const component = (r: ComponentRow) => {
    const why = componentReason(r.reason);
    return (
      <li key={r.id} className="upgrade-release">
        <span className="upgrade-release-action">
          <span className="mono">{r.kind === 'core' ? t('components.core') : r.id}</span>
          {r.pool && <span className="upgrade-site-pool muted">{r.pool}</span>}
        </span>
        <span className="upgrade-release-action">
          <span className="muted mono">{r.version ?? t('build.unknown')}</span>
          {/* What that site is doing about it, from its own heartbeat (ADR-051 Inc.4 decision 18).
              Only the two states an operator can act on are drawn: `running` says the site is
              moving, `failed` says it is stuck. `succeeded` is left blank on purpose — the version
              beside it is the completion signal, and a badge that stays up after the work is done
              reads as work still in flight.

              `message` is written at the site: rendered as a tooltip string, never as a key. The
              key comes from `command`, a closed enum whose labels live in the `system` namespace
              beside the Pollers page's copy — reached across namespaces rather than copied here,
              because `i18nEnumKeys.test.ts` pins only the `system` set and a duplicate would rot
              unchecked. */}
          {r.progress?.state === 'running' && (
            <Badge tone="neutral" title={r.progress.message || undefined}>
              {t(`system:pollers.upgradeStep.${r.progress.command}`)}
            </Badge>
          )}
          {r.progress?.state === 'failed' && (
            <Badge tone="critical" title={r.progress.message || undefined}>
              {t('converge.stuckAt', { step: r.progress.step || '—' })}
            </Badge>
          )}
          {why && <span className="upgrade-why muted">{t(`componentReason.${why}`)}</span>}
          {/* Before the press, and not only inside the dialog: this is the one line on the screen
              that says an upgrade could take a site off the air, and an operator who never opens
              the dialog (they press the running release's own row) would otherwise never meet it.
              Coloured, because every other note in this list is a fact and this one is a risk. */}
          {r.needs_site_prep && (
            <span className="upgrade-why upgrade-why-warn">{t('sitePrep.row')}</span>
          )}
        </span>
      </li>
    );
  };

  const picked = picking;
  const pickedUnprepared = picked === null ? [] : unpreparedSites(rows, selected);
  const pickedDark = picked === null ? [] : darkPools(rows, selected);
  const pickedBack = picked === null ? [] : rows.filter((r) => r.moves_back && selected.includes(r.id));
  const movable = picked === null ? [] : rows.filter((r) => !rowLocked(r, picked));

  return (
    <div>
      {header}

      {/* ── 1. What is running, and what schema it is on ─────────────────────────────────── */}
      <Card title={t('build.heading')}>
        <div className="upgrade-grid">
          <Row label={t('build.coreVersion')} mono>
            {status.current.core_version}
          </Row>
          <Row label={t('build.webuiVersion')} mono>
            {__APP_VERSION__}
          </Row>
          <Row label={t('build.buildProfile')}>
            {t(`buildKind.${kind}`)}
            <div className="upgrade-hint muted">{t(`buildKindHint.${kind}`)}</div>
          </Row>
          <Row label={t('build.sourceRef')} mono>
            {ref ? (
              <span title={status.current.source_ref ?? undefined}>{ref}</span>
            ) : (
              <span className="muted">{t('build.unknown')}</span>
            )}
          </Row>
          <Row label={t('build.hostname')} mono>
            {status.current.hostname ?? <span className="muted">{t('build.unknown')}</span>}
          </Row>
          <Row label={t('build.uptime')}>{uptime(status.current.uptime_seconds)}</Row>
        </div>

        <Sub>{t('schema.heading')}</Sub>
        <div className="upgrade-grid">
          <Row label={t('schema.applied')}>
            {t('schema.appliedValue', { count: status.schema.applied_count })}
          </Row>
          <Row label={t('schema.latest')} mono>
            {status.schema.latest_version ?? <span className="muted">{t('build.unknown')}</span>}
          </Row>
        </div>
      </Card>

      {/* ── 2. The one entrance ──────────────────────────────────────────────────────────── */}
      <Card title={t('mechanism.heading')}>
        {/* Five distinct answers — no mechanism here, not deployed, dead, switched off, ready —
            because they call for five different actions. See `mechanism()`. */}
        {status.updater.present && (
          <label className="upgrade-switch">
            <input
              type="checkbox"
              checked={status.upgrade_enabled}
              disabled={switching || isRunning(status)}
              onChange={() => void toggle(!status.upgrade_enabled)}
            />
            <span>{t('mechanism.switch')}</span>
          </label>
        )}
        {status.updater.present && (
          <p className="upgrade-hint muted">{t('mechanism.switchHint')}</p>
        )}
        {switchError && <p className="upgrade-note">{switchError}</p>}

        {state === 'unsupported' && (
          <>
            <p className="upgrade-note">{t('mechanism.unsupported')}</p>
            <p className="upgrade-hint muted">{t('mechanism.unsupportedHint')}</p>
          </>
        )}
        {state === 'absent' && (
          <>
            <p className="upgrade-note">{t('mechanism.disabled')}</p>
            <p className="upgrade-hint muted">{t('mechanism.disabledHint')}</p>
            {/* A shell command, which only makes sense where there is a composition to run it
                against — hence no counterpart in the `unsupported` block above. */}
            <p className="upgrade-hint muted mono">{t('mechanism.disabledHowTo')}</p>
          </>
        )}
        {state === 'stopped' && (
          <>
            <p className="upgrade-note">{t('mechanism.stopped')}</p>
            <p className="upgrade-hint muted">{t('mechanism.stoppedHint')}</p>
          </>
        )}
        {state === 'paused' && (
          <>
            <p className="upgrade-note">{t('mechanism.paused')}</p>
            {switchPending(status) && (
              <p className="upgrade-hint muted">{t('mechanism.switchPending')}</p>
            )}
          </>
        )}
        {state === 'ready' && (
          <>
            <p className="upgrade-hint muted">
              {t('mechanism.readyFrom', { repo: status.updater.repo ?? '—' })}
            </p>

            {/* When the list was last fetched, and a way to fetch it again. The automatic check
                runs every 24 hours by default, so the hour a release is published is the hour this
                is guaranteed to be stale — which is exactly when someone opens this page. */}
            <div className="upgrade-checked">
              <span className="upgrade-hint muted">
                {checkedAt === null ? (
                  t('mechanism.neverChecked')
                ) : (
                  <span title={formatTimestamp(checkedAt * 1000)}>
                    {t('mechanism.lastChecked', {
                      when: relativeTime(new Date(checkedAt * 1000).toISOString()),
                    })}
                  </span>
                )}
              </span>
              <Button onClick={() => void checkNow()} disabled={checking || inFlight}>
                {checking ? t('mechanism.checking') : t('mechanism.checkNow')}
              </Button>
            </div>
            {checkError && <p className="upgrade-note">{checkError}</p>}

            {newer.length === 0 && (
              <p className="upgrade-note">
                {status.available?.error ? t('mechanism.noRegistry') : t('mechanism.noNewer')}
              </p>
            )}
            {newer.length > 0 && <ul className="upgrade-releases">{newer.map(row)}</ul>}

            {/* Older releases are folded away. They are the long half of the list and the rare
                half of the intent — nobody opens this page to go backwards by default. */}
            {older.length > 0 && (
              <>
                <button
                  type="button"
                  className="upgrade-disclosure"
                  onClick={() => setShowOlder(!showOlder)}
                >
                  {showOlder
                    ? t('rollbackList.hide')
                    : t('rollbackList.show', { count: older.length })}
                </button>
                {showOlder && <ul className="upgrade-releases">{older.map(row)}</ul>}
              </>
            )}
          </>
        )}

        {/* What this deployment is made of. Present whatever state the mechanism is in — a
            deployment that cannot upgrade itself from here still has an inventory worth reading,
            and a remote site can still be brought across, since that touches nothing on this
            host. */}
        <Sub>{t('components.heading')}</Sub>
        <ul className="upgrade-releases upgrade-components">{rows.map(component)}</ul>
        <p className="upgrade-hint muted">{t('components.hint')}</p>
        {/* 🚨 The way in when core is current and only the sites have drifted. It opens the same
            dialog with core's row already excluded, so there is one entrance rather than two.

            **Deliberately outside `state === 'ready'`**, and this is the one placement on the page
            that must not drift: this operation touches nothing on this host, so it needs no central
            updater — gating it on the mechanism would remove a working button from exactly the
            deployments that have remote sites and no updater of their own. `tests/ui/            upgradePick.spec.ts` pins it with the mechanism switched off. */}
        {behind.length > 0 && (
          <div className="upgrade-checked">
            <span className="upgrade-hint muted">
              {t('components.behindHint', {
                count: behind.length,
                version: status.current.core_version,
              })}
            </span>
            <Button
              disabled={cphase.kind === 'running' || cphase.kind === 'starting'}
              onClick={() => open(status.current.core_version)}
            >
              {t('components.bring', { version: status.current.core_version })}
            </Button>
          </div>
        )}
      </Card>

      {/* ── 3. The other way in, for a site with no registry ─────────────────────────────── */}
      {state === 'ready' && status.updater.allow_bundle && (
        <Card title={t('bundle.heading')}>
          <p className="upgrade-hint muted">{t('bundle.intro')}</p>
          <p className="upgrade-hint muted mono">{t('bundle.howTo')}</p>
          <div className="upgrade-bundle">
            <input
              type="file"
              accept=".tar,application/x-tar"
              disabled={uploading || !canUploadBundle(status, pending)}
              onChange={(e) => pickBundle(e.target.files?.[0] ?? null)}
            />
            <input
              type="text"
              className="mono"
              placeholder="v0.2.2"
              value={bundleTag}
              disabled={uploading || !canUploadBundle(status, pending)}
              onChange={(e) => setBundleTag(e.target.value)}
              aria-label={t('bundle.tagLabel')}
            />
            <Button variant="primary" disabled={!bundleReady} onClick={() => void upload()}>
              {t('bundle.button')}
            </Button>
          </div>
          {maxGib !== null && (
            <p className="upgrade-hint muted">{t('bundle.limit', { size: maxGib })}</p>
          )}
          {bundleFile && !tagOk && <p className="upgrade-hint muted">{t('bundle.needTag')}</p>}
          {uploading && (
            <p className="upgrade-hint muted">
              {t('bundle.uploading', { percent: Math.round((uploaded ?? 0) * 100) })}
            </p>
          )}
          {bundleError && <p className="upgrade-note">{bundleError}</p>}
        </Card>
      )}

      {/* ── 4. What is happening, or what happened ───────────────────────────────────────── */}
      {(inFlight || cphase.kind === 'running' || cphase.kind === 'starting') && (
        <Card title={t('run.inFlight')}>
          {inFlight && (
            <>
              <Sub first>{t('converge.thisHost')}</Sub>
              <Progress phase={phase} stale={stale} target={pendingTarget} t={t} />
            </>
          )}
          <Sub first={!inFlight}>{t('converge.heading')}</Sub>
          {cphase.kind === 'running' ? (
            <ConvergeProgress phase={cphase} t={t} />
          ) : (
            <p className="upgrade-note muted">{t('converge.starting')}</p>
          )}
        </Card>
      )}

      {!inFlight && last && (
        <Card title={t('run.heading')}>
          <div className="upgrade-grid">
            <Row label={t('run.state')}>{lastState ? t(`runState.${lastState}`) : last.state}</Row>
            <Row label={t('run.target')} mono>
              {last.target ?? '—'}
            </Row>
            <Row label={t('run.message')}>{last.message ?? '—'}</Row>
            <Row label={t('run.requestedBy')}>{last.requested_by ?? '—'}</Row>
          </div>
        </Card>
      )}

      {/* The poller half's own outcome, kept after it finishes. This is the only place "this site
          did not come back" reaches a screen: a site killed by its own upgrade drops off the live
          registry, so it used to vanish from every list and the deployment read as aligned. */}
      {cphase.kind === 'done' && (
        <Card title={t('converge.lastHeading')}>
          <div className="upgrade-grid">
            <Row label={t('run.state')}>
              {t('converge.outcome', { done: cphase.done, failed: cphase.failed })}
              {cphase.failed > 0 && (
                <>
                  {' '}
                  <Badge tone="critical">{t('converge.attention')}</Badge>
                </>
              )}
            </Row>
            <Row label={t('run.target')} mono>
              {cphase.conv.tag}
            </Row>
            <Row label={t('run.requestedBy')}>{cphase.conv.requested_by}</Row>
          </div>
          <ConvergeProgress phase={cphase} t={t} />
        </Card>
      )}

      {/* ── 5. How far back this can be taken ────────────────────────────────────────────── */}
      <Card title={t('rollback.heading')}>
        {back.kind === 'unrestricted' ? (
          <p className="upgrade-note">{t('rollback.unrestricted')}</p>
        ) : (
          <>
            <p className="upgrade-note">{t('rollback.floored', { minCore: back.minCore })}</p>
            <p className="upgrade-hint muted">
              {t('rollback.flooredReason', { version: back.sinceVersion, reason: back.reason })}
            </p>
            <p className="upgrade-hint muted">{t('rollback.flooredHint')}</p>
          </>
        )}
      </Card>

      {/* ── The dialog: what moves, decided before anything moves ────────────────────────── */}
      {picked !== null && (
        <Modal
          title={t('pick.title', { tag: picked })}
          onClose={() => setPicking(null)}
          size="wide"
          footer={
            <>
              <span className="upgrade-count muted">
                {t('pick.count', { count: selected.length, total: movable.length })}
              </span>
              <Button onClick={() => setPicking(null)} disabled={submitting}>
                {t('apply.cancel')}
              </Button>
              <Button
                variant="danger"
                onClick={() => void submit()}
                disabled={submitting || selected.length === 0}
              >
                {t('pick.confirm', { count: selected.length })}
              </Button>
            </>
          }
        >
          <p className="upgrade-note">{t('pick.intro', { tag: picked })}</p>

          <div className="upgrade-pick">
            <div className="upgrade-pick-head">
              <span />
              <span>{t('pick.component')}</span>
              <span>{t('pick.pool')}</span>
              <span>{t('pick.now')}</span>
              <span />
              <span>{t('pick.next')}</span>
              <span />
            </div>
            {rows.map((r) => {
              const plan = rowPlan(r, picked);
              const locked = rowLocked(r, picked);
              const on = selected.includes(r.id) || (r.co_located && selected.includes(CORE_ID));
              const why = componentReason(r.reason);
              // Only where it would actually happen: a row this press does not move cannot take
              // its site off the air, whatever that site last said.
              const warn = r.needs_site_prep && plan === 'moves';
              return (
                <label
                  key={r.id}
                  className={
                    r.co_located
                      ? 'upgrade-pick-row upgrade-pick-child'
                      : plan === 'moves'
                        ? 'upgrade-pick-row'
                        : 'upgrade-pick-row upgrade-pick-off'
                  }
                >
                  <input
                    type="checkbox"
                    checked={on}
                    disabled={locked}
                    onChange={(e) =>
                      setSelected((cur) =>
                        e.target.checked
                          ? [...cur, r.id]
                          : cur.filter((id) => id !== r.id),
                      )
                    }
                  />
                  <span className="mono">{r.kind === 'core' ? t('components.core') : r.id}</span>
                  <span className="muted">{r.pool ?? '—'}</span>
                  <span className="mono">{r.version ?? '—'}</span>
                  <span className="upgrade-pick-arrow">{plan === 'moves' ? '→' : '—'}</span>
                  <span className="mono">{plan === 'moves' ? picked : '—'}</span>
                  {/* The reason lives in the row, never in a tooltip: a control whose explanation
                      is hover-only is one a touch device never explains (ADR-055 R4).

                      🚨 One span, not two: `.upgrade-pick-row` is a seven-column grid sharing its
                      template with `.upgrade-pick-head`, so an eighth child slides every row out
                      from under its own header — and no test can see that.

                      The site-prep warning outranks the rest when both apply. It is the only entry
                      here that describes damage rather than cost, and `pick.downgrade` below still
                      names a row moved back, so nothing is lost by yielding the cell. */}
                  <span className={warn ? 'upgrade-why upgrade-why-warn' : 'upgrade-why muted'}>
                    {warn
                      ? t('sitePrep.row')
                      : plan === 'already'
                        ? t('componentReason.already', { version: picked })
                        : why
                          ? t(`componentReason.${why}`)
                          : r.moves_back
                            ? t('componentReason.moves_back')
                            : ''}
                  </span>
                </label>
              );
            })}
          </div>

          {selected.includes(CORE_ID) && (
            <p className="upgrade-hint muted">{t('apply.confirmBackup')}</p>
          )}
          {!selected.includes(CORE_ID) && (
            <p className="upgrade-hint muted">{t('pick.coreUntouched')}</p>
          )}
          {/* A poller ahead of core is moved *back*. Named individually, because "upgrade" is what
              the button says and a downgrade is not what it promised. */}
          {pickedBack.length > 0 && (
            <p className="upgrade-note">
              {t('pick.downgrade', {
                count: pickedBack.length,
                names: pickedBack.map((r) => r.id).join(', '),
              })}
            </p>
          )}
          {/* 🚨 The most serious thing this dialog can say, so it sits above the rest of them: the
              others describe a cost the operator is accepting, this one describes a site that may
              not come back. Recomputed from the checkboxes for the same reason `darkPools` is —
              unticking a site has to take it out of the sentence, or the sentence stops being
              about the button.

              The repair is spelled out here rather than pointed at: production is a closed network
              (ADR-045), so the text on the screen is the manual (ADR-055 R5). And it is the repair
              that actually clears this warning — telling an operator to set YAGRA_CERT_DIR would
              make the next upgrade survivable while leaving the row exactly as it is, which is how
              a warning gets read past.

              🚨 Two repairs, cheapest first, and the order is load-bearing. An apply installs the
              site's new composition and then recreates `poller` BY NAME — deliberately, because a
              bare `up -d` would kill the updater running the apply. So the common case after any
              upgrade is a current file and a stale container, which recreating one service fixes
              and which no bundle is needed for. Naming only the bundle would send every such site
              through a token rotation and a trip to the site for a one-line repair. */}
          {pickedUnprepared.length > 0 && (
            <>
              <p className="upgrade-note upgrade-note-warn">
                {t('sitePrep.warning', {
                  count: pickedUnprepared.length,
                  names: pickedUnprepared.join(', '),
                })}
              </p>
              <p className="upgrade-hint muted">{t('sitePrep.fix')}</p>
            </>
          )}
          {/* The consequence an operator cannot find out afterwards except from an alert: no
              maintenance window silences pool coverage, deliberately (ADR-051 decision 13). It is
              recomputed from the checkboxes — a warning that does not follow them reads exactly the
              same when it matters as when it does not. */}
          {pickedDark.length > 0 && (
            <p className="upgrade-note">
              {t('pollers.darkPools', {
                count: pickedDark.length,
                pools: pickedDark.join(', '),
              })}
            </p>
          )}
          {back.kind === 'floored' && selected.includes(CORE_ID) && (
            <p className="upgrade-hint muted">
              {t('apply.confirmFloor', { minCore: back.minCore })}
            </p>
          )}
          {applyError && <p className="form-error">{applyError}</p>}
        </Modal>
      )}
    </div>
  );
}
