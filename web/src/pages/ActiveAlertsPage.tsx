// SPDX-License-Identifier: AGPL-3.0-only
// Active alerts — triage only (§3.2). Per the responsibility split, Yagra detects/correlates/
// suppresses/routes; the acknowledgement *action*, escalation and on-call live in external tools
// (PagerDuty/JSM). So there is NO Ack button — but ack *state* mirrored back from the external
// tool is shown read-only as an "acked" pill (ADR-015, inbound only), whose tooltip already names
// the tool and the person, which is as far as Yagra can honestly point at an external incident.
//
// Two per-alert actions, and both appear only where they would actually work — a control that is
// permanently disabled is a promise the UI cannot keep:
//   - Mute (AckAlerts, operator and above) seeds a node mute with the metric that fired.
//   - Explain (ADR-029) needs a configured provider AND a role that can spend the call.
// Alerts (and acks) arrive live over SSE.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useAlertStream } from '../hooks/useAlertStream';
import { useAlertStore, useAuthStore } from '../store';
import { api } from '../services/api';
import { muteTargetFromAlert, type AlertMuteSeed } from '../lib/suppression';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { RcaModal } from '../components/Rca/RcaModal';
import { AddMuteModal } from '../components/suppression/AddMuteModal';
import { AlertRows } from '../widgets/AlertRows';

export function ActiveAlertsPage() {
  const { t } = useTranslation('alerts');
  useAlertStream();
  const count = useAlertStore((s) => Object.keys(s.alerts).length);
  const role = useAuthStore((s) => s.role);
  const [rcaEnabled, setRcaEnabled] = useState(false);
  const [explaining, setExplaining] = useState<{ node: string; check: string } | null>(null);
  const [muting, setMuting] = useState<AlertMuteSeed | null>(null);

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

  // Both actions are AckAlerts-gated server-side (operator and above) — a viewer would get a 403,
  // so don't show them a button that only ever fails. Explain additionally needs a provider.
  const canSuppress = role === 'operator' || role === 'admin';
  const canExplain = rcaEnabled && canSuppress;

  return (
    <div>
      <PageHeader
        title={t('nav:alerts.active')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.active') }]}
        note={t('active.note', { count })}
      />
      {/* No `Card` around the list: the data-table standard is header → toolbar → rows, and this
          was the one list screen still wrapping its rows in a titled panel (§4.1). */}
      {/* A viewer can do neither, so the actions slot is omitted rather than rendered empty — on
          mobile the cell takes a full-width row of its own, which would otherwise be blank. */}
      <AlertRows
        actions={
          canSuppress
            ? (a, nodeName) => (
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
                    aria-label={t('active.muteHint')}
                    title={t('active.muteHint')}
                    onClick={() => setMuting(muteTargetFromAlert(a, nodeName))}
                  >
                    {t('active.mute')}
                  </Button>
                </>
              )
            : undefined
        }
      />
      {explaining && (
        <RcaModal
          node={explaining.node}
          check={explaining.check}
          onClose={() => setExplaining(null)}
        />
      )}
      {/* The scope is locked to the alert's node, so the dialog never needs the group list. Mutes
          don't change alert state (they suppress *notification*), so there is nothing to reload
          here — the list is SSE-driven and unaffected. */}
      {muting && (
        <AddMuteModal
          initialScope={muting.target}
          initialMetric={muting.metric}
          onClose={() => setMuting(null)}
          onSaved={() => undefined}
        />
      )}
    </div>
  );
}
