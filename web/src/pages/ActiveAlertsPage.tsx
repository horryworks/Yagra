// SPDX-License-Identifier: AGPL-3.0-only
// Active alerts — triage only (§3.2). Per the responsibility split, Yagra detects/correlates/
// suppresses/routes; the acknowledgement *action*, escalation and on-call live in external tools
// (PagerDuty/JSM). So there is NO Ack button — but ack *state* mirrored back from the external
// tool is shown read-only as an "acked" pill (ADR-015, inbound only). The per-alert actions are
// Mute and "open in external tool" — both pending their backends, shown disabled so the intended
// affordance is visible without implying it works — plus "Explain" (ADR-029), which appears only
// where it would actually work: a provider is configured AND the signed-in role can spend a call.
// Alerts (and acks) arrive live over SSE.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useAlertStream } from '../hooks/useAlertStream';
import { useAlertStore, useAuthStore } from '../store';
import { api } from '../services/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { RcaModal } from '../components/Rca/RcaModal';
import { AlertRows } from '../widgets/AlertRows';

export function ActiveAlertsPage() {
  const { t } = useTranslation('alerts');
  useAlertStream();
  const count = useAlertStore((s) => Object.keys(s.alerts).length);
  const role = useAuthStore((s) => s.role);
  const [rcaEnabled, setRcaEnabled] = useState(false);
  const [explaining, setExplaining] = useState<{ node: string; check: string } | null>(null);

  // `rca_enabled` is the server's own answer to "would this button work" — an installation with no
  // provider would 503, so the affordance simply isn't offered there.
  useEffect(() => {
    let cancelled = false;
    api
      .getConfig()
      .then((cfg) => !cancelled && setRcaEnabled(cfg.rca_enabled === true))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  // Generating costs an external call, so it is AckAlerts-gated server-side (operator and above).
  // A viewer would get a 403; don't show them a button that only ever fails.
  const canExplain = rcaEnabled && (role === 'operator' || role === 'admin');

  return (
    <div>
      <PageHeader
        title={t('nav:alerts.active')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.active') }]}
        note={t('active.note', { count })}
      />
      <Card title={t('active.card')}>
        <AlertRows
          actions={(a) => (
            <>
              {canExplain && (
                <Button
                  variant="ghost"
                  aria-label={t('rca:actionHint')}
                  title={t('rca:actionHint')}
                  onClick={() => setExplaining({ node: a.node, check: a.check })}
                >
                  {t('rca:action')}
                </Button>
              )}
              <Button
                variant="ghost"
                disabled
                aria-label={t('active.muteHint')}
                title={t('active.muteHint')}
              >
                {t('active.mute')}
              </Button>
              <Button
                variant="ghost"
                disabled
                aria-label={t('active.openExternalHint')}
                title={t('active.openExternalHint')}
              >
                {t('active.openExternal')}
              </Button>
            </>
          )}
        />
      </Card>
      {explaining && (
        <RcaModal
          node={explaining.node}
          check={explaining.check}
          onClose={() => setExplaining(null)}
        />
      )}
    </div>
  );
}
