// SPDX-License-Identifier: AGPL-3.0-only
// Upgrade (Settings ▸ Upgrade, ADR-050). What this deployment is running, and how far back it can
// be taken. Manage-configuration — it reports build provenance rather than a health counter.
//
// This is Increment 1a: the half that needs no Docker. The privileged updater sidecar, the list of
// available versions and the apply button arrive in 1b/2; until then the page says so plainly
// rather than rendering an empty version list, which would read as "you are up to date".
//
// The judgement lives in `upgradeStatus.ts` so it can be unit-tested — Vitest never runs a .tsx
// (testing.md). What is left here is layout.

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { api, errMsg } from '../services/api';
import type { UpgradeStatus } from '../types/api';
import {
  buildKind,
  canApply,
  isRunning,
  mechanism,
  offerableReleases,
  rollback,
  runState,
  shortRef,
} from './upgradeStatus';
import './UpgradePage.css';

/** How often to re-read the status. Fast while a run is in flight — core restarts during it, so
 *  most of these requests are expected to fail and the page must not treat that as an error. */
const POLL_RUNNING_MS = 5_000;

/** One label/value row, mirroring Settings ▸ About so the two read as one family. */
function Row({ label, children, mono }: { label: string; children: React.ReactNode; mono?: boolean }) {
  return (
    <div className="upgrade-row">
      <div className="upgrade-label muted">{label}</div>
      <div className={mono ? 'upgrade-value mono' : 'upgrade-value'}>{children}</div>
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
  // Sticky: once a run starts, a failed poll means core is restarting — which is the operation
  // working, not an error. Without this the page would flip to "could not read" mid-upgrade.
  const everSeen = useRef(false);

  const load = useCallback(async () => {
    try {
      const s = await api.getUpgradeStatus();
      everSeen.current = true;
      setStatus(s);
      setFailed(false);
    } catch {
      if (!everSeen.current) setFailed(true);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // While a run is in flight, keep polling *through* the outage. `status` is deliberately not in
  // the dependency list beyond the flag: re-creating the interval on every tick would reset it.
  const running = status ? isRunning(status) : false;
  useEffect(() => {
    if (!running) return undefined;
    const h = window.setInterval(() => void load(), POLL_RUNNING_MS);
    return () => window.clearInterval(h);
  }, [running, load]);

  const apply = async (tag: string) => {
    setSubmitting(true);
    setApplyError(null);
    try {
      await api.applyUpgrade(tag);
      setConfirming(null);
      await load();
    } catch (e) {
      setApplyError(errMsg(e, t('apply.failed')));
    } finally {
      setSubmitting(false);
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

  const kind = buildKind(status.current.build_profile);
  const ref = shortRef(status.current.source_ref);
  const back = rollback(status);
  const state = mechanism(status);
  const releases = offerableReleases(status);
  const last = status.last_run;
  const lastState = runState(last?.state);

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
            <p className="upgrade-hint muted">{t('rollback.unrestrictedHint')}</p>
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
        {/* Three distinct answers — never deployed, deployed but dead, ready — because they call
            for three different actions. See `mechanism()`. */}
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
        {state === 'ready' && (
          <>
            <p className="upgrade-hint muted">
              {t('mechanism.readyFrom', { repo: status.updater.repo ?? '—' })}
            </p>
            {releases.length === 0 ? (
              <p className="upgrade-note">
                {status.available?.error ? t('mechanism.noRegistry') : t('mechanism.noNewer')}
              </p>
            ) : (
              <ul className="upgrade-releases">
                {releases.map((tag) => (
                  <li key={tag} className="upgrade-release">
                    <span className="mono">{tag}</span>
                    <Button
                      variant="primary"
                      disabled={!canApply(status)}
                      onClick={() => {
                        setApplyError(null);
                        setConfirming(tag);
                      }}
                    >
                      {t('apply.button')}
                    </Button>
                  </li>
                ))}
              </ul>
            )}
          </>
        )}
      </Card>

      {last && (
        <Card title={t('run.heading')}>
          <div className="upgrade-grid">
            <Row label={t('run.state')}>
              {lastState ? t(`runState.${lastState}`) : last.state}
              {last.step ? ` — ${last.step}` : ''}
            </Row>
            <Row label={t('run.target')} mono>
              {last.target ?? '—'}
            </Row>
            <Row label={t('run.message')}>{last.message ?? '—'}</Row>
            <Row label={t('run.requestedBy')}>{last.requested_by ?? '—'}</Row>
          </div>
          {/* The one thing the operator must be told before their session drops. */}
          {isRunning(status) && <p className="upgrade-hint muted">{t('run.disconnectWarning')}</p>}
        </Card>
      )}

      {confirming && (
        <Modal
          title={t('apply.confirmTitle', { tag: confirming })}
          onClose={() => setConfirming(null)}
          footer={
            <>
              <Button onClick={() => setConfirming(null)} disabled={submitting}>
                {t('apply.cancel')}
              </Button>
              <Button variant="danger" onClick={() => void apply(confirming)} disabled={submitting}>
                {t('apply.confirm')}
              </Button>
            </>
          }
        >
          <p className="upgrade-note">{t('apply.confirmBody', { tag: confirming })}</p>
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
