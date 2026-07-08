// Active alerts — triage only (§3.2). Per the responsibility split, Yagra detects/correlates/
// suppresses/routes; the acknowledgement *action*, escalation and on-call live in external tools
// (PagerDuty/JSM). So there is NO Ack button — but ack *state* mirrored back from the external
// tool is shown read-only as an "acked" pill (ADR-015, inbound only). The only per-alert actions
// are Mute and "open in external tool" — both pending their backends, shown disabled so the
// intended affordance is visible without implying it works. Alerts (and acks) arrive live over SSE.

import { useTranslation } from 'react-i18next';
import { useAlertStream } from '../hooks/useAlertStream';
import { useAlertStore } from '../store';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { AlertRows } from '../widgets/AlertRows';

export function ActiveAlertsPage() {
  const { t } = useTranslation('alerts');
  useAlertStream();
  const count = useAlertStore((s) => Object.keys(s.alerts).length);

  return (
    <div>
      <PageHeader
        title={t('nav:alerts.active')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.active') }]}
        note={t('active.note', { count })}
      />
      <Card title={t('active.card')}>
        <AlertRows
          actions={() => (
            <>
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
    </div>
  );
}
