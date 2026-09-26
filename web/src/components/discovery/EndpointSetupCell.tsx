// SPDX-License-Identifier: AGPL-3.0-only
// The "Monitoring setup" cell of one unregistered endpoint (ADR-179 増分 2 決定 7, 増分 3 決定 3):
// Detect alone until it has answered, then the two dropdowns — filled when it found something,
// empty to pick by hand when it did not. Drawn by Discovery ▸ Unregistered devices and by the
// Node ▸ Neighbors tab, over the same `useEndpointSetup` state.
//
// The caller draws it only with manage_config (ADR-056): every control in it is a write.

import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import type { CredentialSummary, ProfileSummary } from '../../types/api';
import { detectPhase } from '../../pages/discoveredEndpoints';
import type { EndpointSetup, SetupTarget } from '../../lib/useEndpointSetup';
import { Button } from '../ui/Button';
import { Select } from '../ui/Field';
import './EndpointSetupCell.css';

interface Props {
  target: SetupTarget;
  setup: EndpointSetup;
  profiles: ProfileSummary[];
  creds: CredentialSummary[];
  /** How many credentials Detect will try — the idle hint names the number, and warns at zero. */
  probeCredCount: number;
  onMonitor: () => void;
  /** When Detect gets no answer, this replaces the hand-pick form: a device that must not be
   *  registered by hand without SNMP (an access point, ADR-179 増分 3 決定 2 ③) is told where it
   *  is added instead. Omitted, the form is offered as on Discovery. */
  noAnswer?: ReactNode;
  className?: string;
}

export function EndpointSetupCell({
  target,
  setup,
  profiles,
  creds,
  probeCredCount,
  onMonitor,
  noAnswer,
  className,
}: Props) {
  const { t } = useTranslation('monitoring');
  const phase = detectPhase(setup.detect[target.id]);
  const found = phase === 'found';
  const busy = setup.busyId != null;
  const sel = setup.selection(target.id);
  const line = setup.detectLine(target.id);
  const root = className ? `ep-setup ${className}` : 'ep-setup';

  if (phase === 'idle' || phase === 'running') {
    return (
      <div className={root}>
        <div className="ep-setup-actions">
          <Button
            variant="primary"
            disabled={phase === 'running' || busy}
            title={t('discovery.seen.detect.hint')}
            onClick={() => void setup.detectOne(target)}
          >
            {phase === 'running' ? t('discovery.seen.detect.running') : t('discovery.seen.detect.button')}
          </Button>
          <span className={probeCredCount === 0 ? 'ep-setup-hint warn' : 'ep-setup-hint'}>
            {phase === 'running'
              ? t('discovery.seen.detect.trying', { count: probeCredCount })
              : probeCredCount === 0
                ? t('discovery.seen.detect.noCreds')
                : t('discovery.seen.detect.idle', { count: probeCredCount })}
          </span>
        </div>
      </div>
    );
  }

  if (!found && noAnswer != null) {
    return (
      <div className={root}>
        <span className="ep-setup-detect warn">
          <WarnMark />
          {noAnswer}
        </span>
        <div className="ep-setup-actions">
          <Button disabled={busy} onClick={() => void setup.detectOne(target)}>
            {t('discovery.seen.detect.retry')}
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div className={root}>
      {line != null && (
        // Maker and model are device-supplied: rendered as text. The mark is paired with words,
        // never colour alone.
        <span className={found ? 'ep-setup-detect ok' : 'ep-setup-detect warn'}>
          {found ? (
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M20 6 9 17l-5-5" />
            </svg>
          ) : (
            <WarnMark />
          )}
          {line}
        </span>
      )}
      <div className="ep-setup-form">
        <label className={found ? 'ep-setup-field detected' : 'ep-setup-field'}>
          {found ? t('discovery.seen.detect.profileDetected') : t('discovery.cols.profile')}
          <Select
            value={sel.profile_id}
            disabled={busy}
            onChange={(ev) => setup.choose(target.id, { profile_id: ev.target.value })}
          >
            <option value="">{t('discovery.seen.detect.noProfile')}</option>
            {profiles.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </Select>
        </label>
        <label className={found ? 'ep-setup-field detected' : 'ep-setup-field'}>
          {found ? t('discovery.seen.detect.credentialAnswered') : t('discovery.cols.credential')}
          <Select
            value={sel.credential_id}
            disabled={busy}
            onChange={(ev) => setup.choose(target.id, { credential_id: ev.target.value })}
          >
            <option value="">{t('discovery.none')}</option>
            {creds.map((cr) => (
              <option key={cr.id} value={cr.id}>
                {cr.name}
              </option>
            ))}
          </Select>
        </label>
        {!found && (
          <Button disabled={busy} onClick={() => void setup.detectOne(target)}>
            {t('discovery.seen.detect.retry')}
          </Button>
        )}
        <Button variant={found ? 'primary' : 'outline'} disabled={busy} onClick={onMonitor}>
          {t('discovery.seen.monitor')}
        </Button>
      </div>
    </div>
  );
}

function WarnMark() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 9v4" />
      <path d="M12 17h.01" />
      <path d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" />
    </svg>
  );
}
