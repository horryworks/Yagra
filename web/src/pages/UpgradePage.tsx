// SPDX-License-Identifier: AGPL-3.0-only
// Upgrade (Settings ▸ Upgrade, ADR-050). What this deployment is running, and how far back it can
// be taken. Manage-configuration — it reports build provenance rather than a health counter.
//
// The privileged updater sidecar is off by default, so the page says so plainly rather than
// rendering an empty version list, which would read as "you are up to date".
//
// The judgement lives in `upgradeStatus.ts` so it can be unit-tested — Vitest never runs a .tsx
// (testing.md). What is left here is layout.

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { ProgressBar } from '../components/ui/ProgressBar';
import { api, errMsg } from '../services/api';
import { formatTimestamp, relativeTime } from '../lib/format';
import type { UpgradeStatus } from '../types/api';
import type { Offer, RunPhase } from './upgradeStatus';
import {
  buildKind,
  bundleTagFromFilename,
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
  shortRef,
  shouldPoll,
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
  const [confirming, setConfirming] = useState<string | null>(null);
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
    } catch {
      if (!everSeen.current) setFailed(true);
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

  const apply = async (tag: string) => {
    setSubmitting(true);
    setApplyError(null);
    try {
      const accepted = await api.applyUpgrade(tag);
      setPending(accepted.id);
      setRequestedTag(tag);
      setConfirming(null);
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
          onClick={() => {
            setApplyError(null);
            setConfirming(offer.tag);
          }}
        >
          {t(`offerAction.${offer.direction}`)}
        </Button>
      </div>
    </li>
  );

  const kind = buildKind(status.current.build_profile);
  const ref = shortRef(status.current.source_ref);
  const back = rollback(status);
  const state = mechanism(status);
  const last = status.last_run;
  const lastState = runState(last?.state);
  const newer = upgrades(status);
  const older = rollbacks(status);
  // The dialog has to say the same thing the button did. Falls back to `upgrade` only if the offer
  // vanished between the click and the render, which cannot happen without a reload.
  const confirmingDir =
    status.offers.find((o) => o.tag === confirming)?.direction ?? 'upgrade';
  const phase = runPhase(status, pending);
  const inFlight = phase.kind === 'starting' || phase.kind === 'running';
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

  return (
    <div>
      {header}

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
      </Card>

      <Card title={t('schema.heading')}>
        <div className="upgrade-grid">
          <Row label={t('schema.applied')}>
            {t('schema.appliedValue', { count: status.schema.applied_count })}
          </Row>
          <Row label={t('schema.latest')} mono>
            {status.schema.latest_version ?? <span className="muted">{t('build.unknown')}</span>}
          </Row>
        </div>
      </Card>

      <Card title={t('rollback.heading')}>
        {back.kind === 'unrestricted' ? (
          <>
            <p className="upgrade-note">{t('rollback.unrestricted')}</p>
          </>
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

      <Card title={t('mechanism.heading')}>
        {/* Four distinct answers — not deployed, dead, switched off, ready — because they call for
            four different actions. See `mechanism()`. */}
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

        {state === 'absent' && (
          <>
            <p className="upgrade-note">{t('mechanism.disabled')}</p>
            <p className="upgrade-hint muted">{t('mechanism.disabledHint')}</p>
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
      </Card>

      {/* Only where the deployment has opted in. A site with a registry never sees this — and a
          site without one has no other way in, since the release list above will be empty. */}
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
            <Button
              variant="primary"
              disabled={!bundleReady}
              onClick={() => void upload()}
            >
              {t('bundle.button')}
            </Button>
          </div>
          {maxGib !== null && (
            <p className="upgrade-hint muted">{t('bundle.limit', { size: maxGib })}</p>
          )}
          {bundleFile && !tagOk && (
            <p className="upgrade-hint muted">{t('bundle.needTag')}</p>
          )}
          {uploading && (
            <p className="upgrade-hint muted">
              {t('bundle.uploading', { percent: Math.round((uploaded ?? 0) * 100) })}
            </p>
          )}
          {bundleError && <p className="upgrade-note">{bundleError}</p>}
        </Card>
      )}

      {inFlight && (
        <Card title={t('run.inFlight')}>
          <Progress phase={phase} stale={stale} target={pendingTarget} t={t} />
        </Card>
      )}

      {!inFlight && last && (
        <Card title={t('run.heading')}>
          <div className="upgrade-grid">
            <Row label={t('run.state')}>
              {lastState ? t(`runState.${lastState}`) : last.state}
            </Row>
            <Row label={t('run.target')} mono>
              {last.target ?? '—'}
            </Row>
            <Row label={t('run.message')}>{last.message ?? '—'}</Row>
            <Row label={t('run.requestedBy')}>{last.requested_by ?? '—'}</Row>
          </div>
        </Card>
      )}

      {confirming && (
        <Modal
          title={t(`apply.confirmTitle.${confirmingDir}`, { tag: confirming })}
          onClose={() => setConfirming(null)}
          footer={
            <>
              <Button onClick={() => setConfirming(null)} disabled={submitting}>
                {t('apply.cancel')}
              </Button>
              <Button variant="danger" onClick={() => void apply(confirming)} disabled={submitting}>
                {t(`apply.confirm.${confirmingDir}`)}
              </Button>
            </>
          }
        >
          <p className="upgrade-note">
            {t(`apply.confirmBody.${confirmingDir}`, { tag: confirming })}
          </p>
          <p className="upgrade-hint muted">{t('apply.confirmBackup')}</p>
          {back.kind === 'floored' && (
            <p className="upgrade-hint muted">
              {t('apply.confirmFloor', { minCore: back.minCore })}
            </p>
          )}
          {applyError && <p className="upgrade-hint">{applyError}</p>}
        </Modal>
      )}
    </div>
  );
}
