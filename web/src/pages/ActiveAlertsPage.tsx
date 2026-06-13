// Active alerts — triage only (§3.2). Per the responsibility split, Yagra detects/correlates/
// suppresses/routes; acknowledgement, escalation and on-call live in external tools
// (PagerDuty/JSM). So there is NO Ack button and NO acked column. The only per-alert actions
// are Mute and "open in external tool" — both pending their backends, shown disabled so the
// intended affordance is visible without implying it works. Alerts arrive live over SSE.

import { useAlertStream } from '../hooks/useAlertStream';
import { useAlertStore } from '../store';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { AlertRows } from '../widgets/AlertRows';

export function ActiveAlertsPage() {
  useAlertStream();
  const count = useAlertStore((s) => Object.keys(s.alerts).length);

  return (
    <div>
      <PageHeader
        title="Active alerts"
        trail={[{ label: 'Alerts' }, { label: 'Active alerts' }]}
        note={`${count} active · triage view (ack & escalation are handled in your external tool)`}
      />
      <Card title="Active">
        <AlertRows
          empty="No active alerts."
          actions={() => (
            <>
              <Button
                variant="ghost"
                disabled
                aria-label="Per-alert quick-mute is coming soon — use Alerts ▸ Mutes for now"
                title="Per-alert quick-mute is coming soon — use Alerts ▸ Mutes for now"
              >
                Mute
              </Button>
              <Button
                variant="ghost"
                disabled
                aria-label="No external routing integration is configured yet (PagerDuty / JSM)"
                title="No external routing integration is configured yet (PagerDuty / JSM)"
              >
                Open external
              </Button>
            </>
          )}
        />
      </Card>
    </div>
  );
}
