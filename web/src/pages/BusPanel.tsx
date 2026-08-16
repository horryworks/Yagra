// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Pollers ▸ Remote pollers (ADR-065). The certificate a remote site pins, and the switch
// that lets one connect at all.
//
// This panel exists because the previous answer to "how do I add a site?" was an `openssl`
// invocation, a hand edit of two blocks of docker-compose.deploy.yml, and one shared password —
// and the hand edits are erased by the next upgrade, after which the central stack keeps working
// and every remote poller silently stops connecting. The screen is the fix, not a convenience.
//
// All judgement lives in `lib/busCert.ts` so it can be tested (Vitest never executes a `.tsx`).
// What is left here is layout and the three dialogs.

import { useCallback, useEffect, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import type { BusRemoteAccepted, BusStatus } from '../types/api';
import { Badge } from '../components/ui/Badge';
import { Button } from '../components/ui/Button';
import { Card } from '../components/ui/Card';
import { Modal } from '../components/ui/Modal';
import { TextInput, FieldHint } from '../components/ui/Field';
import { busCertState, namesNotCovered, parseBusNames } from '../lib/busCert';
import { formatExactTime } from '../lib/format';

/** Copy to the clipboard and flash a confirmation. Local because the shared table-cell copy helper
 *  is about entity ids; this is about a secret shown once, where the operator needs to *see* that
 *  the copy happened before they close the dialog. */
function useCopy() {
  const [copied, setCopied] = useState<string | null>(null);
  const copy = useCallback((text: string, mark: string) => {
    void navigator.clipboard?.writeText(text);
    setCopied(mark);
    setTimeout(() => setCopied(null), 1400);
  }, []);
  return { copied, copy };
}

/** Reissue the certificate with names an operator supplies. No restart: the stored certificate
 *  changes immediately and the bus serves it when it is next recreated, which the dialog says. */
function ReissueModal({
  currentSans,
  onClose,
  onDone,
}: {
  currentSans: string[];
  onClose: () => void;
  onDone: (s: BusStatus) => void;
}) {
  const { t } = useTranslation('system');
  // Seeded with what the certificate already covers, minus the internal defaults the server adds
  // back on its own. Starting empty would make "reissue to add one site" read as "replace the list",
  // which is how a working site loses its name.
  const [text, setText] = useState(
    currentSans.filter((s) => !['nats', 'localhost', '127.0.0.1', '::1'].includes(s)).join(', '),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const save = () => {
    setBusy(true);
    setError(null);
    api
      .regenerateBusCert(parseBusNames(text))
      .then(onDone)
      .catch((e) => setError(errMsg(e, t('pollers.bus.reissue.failed'))))
      .finally(() => setBusy(false));
  };

  return (
    <Modal
      title={t('pollers.bus.reissue.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={busy}>
            {t('pollers.bus.reissue.submit')}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <p className="modal-confirm-text">{t('pollers.bus.reissue.intro')}</p>
        <label className="form-label">
          {t('pollers.bus.names.label')}
          <TextInput
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder="yagra.example.net, 203.0.113.10"
            autoFocus
          />
        </label>
        <FieldHint>{t('pollers.bus.names.hint')}</FieldHint>
        <p className="form-hint">{t('pollers.bus.reissue.afterward')}</p>
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}

/** Turn acceptance on or off. The confirmation is the point: this recreates the bus, so monitoring
 *  stops and this session's core restarts underneath the operator. */
function SwitchModal({
  enabling,
  currentSans,
  onClose,
  onAccepted,
}: {
  enabling: boolean;
  currentSans: string[];
  onClose: () => void;
  onAccepted: (a: BusRemoteAccepted) => void;
}) {
  const { t } = useTranslation('system');
  const [text, setText] = useState(
    currentSans.filter((s) => !['nats', 'localhost', '127.0.0.1', '::1'].includes(s)).join(', '),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const names = parseBusNames(text);
  const ready = !enabling || names.length > 0;

  const go = () => {
    setBusy(true);
    setError(null);
    api
      .setBusRemote(enabling, names)
      .then(onAccepted)
      .catch((e) => setError(errMsg(e, t('pollers.bus.switchFailed'))))
      .finally(() => setBusy(false));
  };

  return (
    <Modal
      title={enabling ? t('pollers.bus.enable.title') : t('pollers.bus.disable.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant={enabling ? 'primary' : 'danger'} onClick={go} disabled={busy || !ready}>
            {enabling ? t('pollers.bus.enable.submit') : t('pollers.bus.disable.submit')}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <p className="modal-confirm-text">
          {enabling ? t('pollers.bus.enable.intro') : t('pollers.bus.disable.intro')}
        </p>
        {enabling && (
          <>
            <label className="form-label">
              {t('pollers.bus.names.label')}
              <TextInput
                value={text}
                onChange={(e) => setText(e.target.value)}
                placeholder="yagra.example.net, 203.0.113.10"
                autoFocus
              />
            </label>
            <FieldHint error={!ready}>
              {ready ? t('pollers.bus.names.hint') : t('pollers.bus.names.required')}
            </FieldHint>
          </>
        )}
        {/* The cost, stated before the click rather than discovered after it. */}
        <p className="form-hint">{t('pollers.bus.outage')}</p>
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}

/** What the site needs, shown once. Closing this dialog is the last time the secret exists. */
function HandoffModal({
  accepted,
  onClose,
}: {
  accepted: BusRemoteAccepted;
  onClose: () => void;
}) {
  const { t } = useTranslation('system');
  const { copied, copy } = useCopy();
  return (
    <Modal
      title={t('pollers.bus.handoff.title')}
      onClose={onClose}
      footer={
        <Button variant="primary" onClick={onClose}>
          {t('pollers.bus.handoff.done')}
        </Button>
      }
    >
      <div className="form-stack">
        <p className="modal-confirm-text">{t('pollers.bus.handoff.intro')}</p>
        {accepted.poller_secret && (
          <div className="modal-field">
            <label className="modal-field-label">{t('pollers.bus.handoff.secret')}</label>
            <div className="poller-copyrow">
              <code className="poller-snippet mono">{accepted.poller_secret}</code>
              <Button variant="outline" onClick={() => copy(accepted.poller_secret ?? '', 'secret')}>
                {copied === 'secret' ? t('common:copy.copied') : t('pollers.register.copy')}
              </Button>
            </div>
            <FieldHint error>{t('pollers.bus.handoff.onceOnly')}</FieldHint>
          </div>
        )}
        {accepted.ca_certificate && (
          <div className="modal-field">
            <label className="modal-field-label">{t('pollers.bus.handoff.ca')}</label>
            <div className="poller-copyrow">
              <pre className="poller-snippet mono">{accepted.ca_certificate}</pre>
              <Button
                variant="outline"
                onClick={() => copy(accepted.ca_certificate ?? '', 'ca')}
              >
                {copied === 'ca' ? t('common:copy.copied') : t('pollers.register.copy')}
              </Button>
            </div>
          </div>
        )}
        <p className="form-hint">{t('pollers.bus.handoff.restarting')}</p>
      </div>
    </Modal>
  );
}

/** The panel. Rendered above the pool summary strip on Settings ▸ Pollers. */
export function BusPanel() {
  const { t } = useTranslation('system');
  // The bus is deployment topology, and the switch reaches the container holding the Docker socket.
  const canSystem = useCan('manage_system');
  const [status, setStatus] = useState<BusStatus | null>(null);
  // `null` while loading and `false` after a refused read, so the panel can stay silent on a
  // deployment that has no bus certificate store rather than showing an error beside a working list.
  const [available, setAvailable] = useState<boolean | null>(null);
  const [reissuing, setReissuing] = useState(false);
  const [switching, setSwitching] = useState<boolean | null>(null);
  const [accepted, setAccepted] = useState<BusRemoteAccepted | null>(null);

  useEffect(() => {
    if (!canSystem) {
      setAvailable(false);
      return;
    }
    let live = true;
    api
      .getBus()
      .then((s) => {
        if (!live) return;
        setStatus(s);
        setAvailable(true);
      })
      .catch(() => live && setAvailable(false));
    return () => {
      live = false;
    };
  }, [canSystem]);

  if (!canSystem || available === false) return null;

  const cert = status?.certificate ?? null;
  const state = busCertState(cert);
  const enabled = status?.remote_enabled ?? false;
  const extraSans = (cert?.sans ?? []).filter(
    (s) => !['nats', 'localhost', '127.0.0.1', '::1'].includes(s),
  );
  // The question the panel exists to answer before somebody drives to a site.
  const uncovered = namesNotCovered(cert, []);

  return (
    <Card
      className="bus-panel"
      title={t('pollers.bus.title')}
      actions={
        status && (
          <>
            <Button variant="outline" onClick={() => setReissuing(true)}>
              {t('pollers.bus.reissue.action')}
            </Button>
            {status.can_switch && (
              <Button
                variant={enabled ? 'outline' : 'primary'}
                onClick={() => setSwitching(!enabled)}
              >
                {enabled ? t('pollers.bus.disable.action') : t('pollers.bus.enable.action')}
              </Button>
            )}
          </>
        )
      }
    >
      <p className="muted">{t('pollers.bus.note')}</p>

      <p>
        <Badge tone={enabled ? 'up' : 'neutral'}>
          {enabled ? t('pollers.bus.state.encrypted') : t('pollers.bus.state.internal')}
        </Badge>{' '}
        <span className="muted">
          {enabled ? t('pollers.bus.state.encryptedNote') : t('pollers.bus.state.internalNote')}
        </span>
      </p>

      {status && !status.can_switch && (
        <p className="form-hint">{t('pollers.bus.noSwitch')}</p>
      )}

      {cert ? (
        <div className="form-stack">
          <p>
            <span className="muted">{t('pollers.bus.cert.sans')}: </span>
            {extraSans.length > 0 ? (
              <span className="mono">{extraSans.join(', ')}</span>
            ) : (
              <span className="muted">{t('pollers.bus.cert.internalOnly')}</span>
            )}
          </p>
          <p className="muted">
            {t('pollers.bus.cert.expires', {
              when: formatExactTime(cert.not_after),
              days: cert.expires_in_days,
            })}
          </p>
          <p className="muted mono" title={cert.fingerprint_sha256}>
            {t('pollers.bus.cert.fingerprint')}: {cert.fingerprint_sha256.slice(0, 32)}…
          </p>
          {/* One line, worst first — see `busCertState`. */}
          {state !== 'ok' && (
            <p className={state === 'expiring' ? 'form-hint' : 'form-error'}>
              {t(`pollers.bus.cert.warn.${state}`)}
            </p>
          )}
          {uncovered.length > 0 && (
            <p className="form-error">
              {t('pollers.bus.cert.uncovered', { names: uncovered.join(', ') })}
            </p>
          )}
        </div>
      ) : (
        <p className="muted">{t('pollers.bus.cert.absent')}</p>
      )}

      <p className="form-hint">
        <Trans t={t} i18nKey="pollers.bus.siteHint" components={{ c: <span className="mono" /> }} />
      </p>

      {reissuing && (
        <ReissueModal
          currentSans={cert?.sans ?? []}
          onClose={() => setReissuing(false)}
          onDone={(s) => {
            setStatus(s);
            setReissuing(false);
          }}
        />
      )}
      {switching !== null && (
        <SwitchModal
          enabling={switching}
          currentSans={cert?.sans ?? []}
          onClose={() => setSwitching(null)}
          onAccepted={(a) => {
            setSwitching(null);
            setAccepted(a);
          }}
        />
      )}
      {accepted && <HandoffModal accepted={accepted} onClose={() => setAccepted(null)} />}
    </Card>
  );
}
