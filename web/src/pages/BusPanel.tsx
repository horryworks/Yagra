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

import { useEffect, useState } from 'react';
import { useCopy } from '../lib/useCopy';
import { Trans, useTranslation } from 'react-i18next';
import { api } from '../services/api';
import { useCan } from '../store';
import type { BusRemoteAccepted, BusStatus } from '../types/api';
import { Badge } from '../components/ui/Badge';
import { Button } from '../components/ui/Button';
import { Card } from '../components/ui/Card';
import { Modal } from '../components/ui/Modal';
import { TextInput, FieldError } from '../components/ui/Field';
import { StepFrame } from '../components/ui/StepFrame';
import { busCertState, externalBusNames, parseBusNames } from '../lib/busCert';
import { formatExactTime } from '../lib/format';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { FormError, FormFooter } from '../components/ui/FormFooter';

/** The confirmation stays up a little longer than elsewhere: this is a secret shown once, and the
 *  operator needs to *see* that the copy happened before they close the dialog. */
const SECRET_COPY_FLASH_MS = 1400;

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
  const [text, setText] = useState(externalBusNames(currentSans).join(', '));
  const form = useSubmit({ errorFallback: t('pollers.bus.reissue.failed'), onDone });

  const save = () =>
    form.submit(() => api.regenerateBusCert(parseBusNames(text)).then((s) => done(s)));

  return (
    <Modal
      title={t('pollers.bus.reissue.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={save}
          submitLabel={t('pollers.bus.reissue.submit')}
        />
      }
    >
      <div className="form-stack">
        <label className="form-label">
          {t('pollers.bus.names.label')}
          <TextInput
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder="yagra.example.net, 203.0.113.10"
            autoFocus
          />
        </label>
        {/* The dialog's one sentence (ADR-200): what the sites lose until they are handed the
            new file. That the bus serves it only after a restart shows on the panel as a badge. */}
        <p className="modal-confirm-text">{t('pollers.bus.reissue.afterward')}</p>
        <FormError form={form} />
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
  const [text, setText] = useState(externalBusNames(currentSans).join(', '));
  const form = useSubmit({ errorFallback: t('pollers.bus.switchFailed'), onDone: onAccepted });
  const names = parseBusNames(text);
  const ready = !enabling || names.length > 0;

  const go = () => form.submit(() => api.setBusRemote(enabling, names).then((a) => done(a)));

  return (
    <Modal
      title={enabling ? t('pollers.bus.enable.title') : t('pollers.bus.disable.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={go}
          submitLabel={enabling ? t('pollers.bus.enable.submit') : t('pollers.bus.disable.submit')}
          canSubmit={ready}
          variant={enabling ? 'primary' : 'danger'}
        />
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
            {!ready && <FieldError>{t('pollers.bus.names.required')}</FieldError>}
          </>
        )}
        {/* The cost, stated before the click rather than discovered after it. */}
        <p className="form-warning">{t('pollers.bus.outage')}</p>
        <FormError form={form} />
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
  const { copied, copy } = useCopy(SECRET_COPY_FLASH_MS);
  return (
    <Modal
      title={t('pollers.bus.handoff.title')}
      resizeId="busHandoff"
      onClose={onClose}
      footer={
        <Button variant="primary" onClick={onClose}>
          {t('pollers.bus.handoff.done')}
        </Button>
      }
    >
      <div className="form-stack">
        {accepted.poller_secret && (
          <div className="modal-field">
            <label className="modal-field-label">{t('pollers.bus.handoff.secret')}</label>
            <div className="poller-copyrow">
              <code className="poller-snippet mono">{accepted.poller_secret}</code>
              <Button variant="outline" onClick={() => copy(accepted.poller_secret ?? '', 'secret')}>
                {copied === 'secret' ? t('common:copy.copied') : t('pollers.register.copy')}
              </Button>
            </div>
            <FieldError>{t('pollers.bus.handoff.onceOnly')}</FieldError>
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
        <p className="form-status" role="status">
          {t('pollers.bus.handoff.restarting')}
        </p>
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
  const extraSans = externalBusNames(cert?.sans);
  // "Can a site at that address connect?" is answered in the kit dialog (`PollerTokenModal`),
  // where the address is typed. The panel has no list of site addresses to compare against —
  // none is stored — so the line it once drew here compared against an empty list and never fired.

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
      <p>
        <Badge tone={enabled ? 'up' : 'neutral'}>
          {enabled ? t('pollers.bus.state.encrypted') : t('pollers.bus.state.internal')}
        </Badge>{' '}
        <span className="muted">
          {enabled ? t('pollers.bus.state.encryptedNote') : t('pollers.bus.state.internalNote')}
        </span>
        {/* A reissued certificate is stored at once and served from the next bus restart. That
            is a state, so it is a badge beside the bus's other state rather than a sentence in
            the dialog that made it (ADR-200). */}
        {state === 'not_materialized' && (
          <>
            {' '}
            <Badge tone="warning">{t('pollers.bus.cert.pending')}</Badge>
          </>
        )}
      </p>

      {status && !status.can_switch && (
        <p className="form-status">{t('pollers.bus.noSwitch')}</p>
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
          {/* One line, worst first — see `busCertState`. "Pending restart" is the badge above. */}
          {state !== 'ok' && state !== 'not_materialized' && (
            <p className={state === 'expiring' ? 'form-warning' : 'form-error'}>
              {t(`pollers.bus.cert.warn.${state}`)}
            </p>
          )}
        </div>
      ) : (
        <p className="muted">{t('pollers.bus.cert.absent')}</p>
      )}

      {/* The manual procedure at a site, closed until wanted (ADR-200 kind d). The kit from
          "Register poller" makes it unnecessary for most sites. */}
      <StepFrame
        summary={t('pollers.bus.steps.title')}
        steps={[
          t('pollers.bus.steps.s1'),
          <Trans
            key="s2"
            t={t}
            i18nKey="pollers.bus.steps.s2"
            components={{ c: <span className="mono" /> }}
          />,
          t('pollers.bus.steps.s3'),
        ]}
      />

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
