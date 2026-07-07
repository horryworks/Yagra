// Settings ▸ Integrations. Catalog of external systems Yagra can monitor through a vendor cloud
// API. Today the only integration is Cisco Meraki; the page is a card grid so more vendors slot in
// without restructuring. Each connected integration links to its own detail page (the Meraki page
// moved to /settings/integrations/meraki). The status chip is derived, not authored: it reflects
// the live org count + polling switch so the operator sees "Connected · 2 orgs" at a glance.

import { useEffect, useState } from 'react';
import { Link } from 'react-router-dom';
import { api, ApiError } from '../../services/api';
import { PageHeader } from '../../components/ui/PageHeader';
import { Card } from '../../components/ui/Card';
import './IntegrationsCatalogPage.css';

type MerakiStatus =
  | { kind: 'loading' }
  | { kind: 'unavailable' }
  | { kind: 'not-configured' }
  | { kind: 'connected'; orgs: number; pollingOn: boolean };

function merakiStatusLabel(s: MerakiStatus): string {
  switch (s.kind) {
    case 'loading':
      return 'Checking…';
    case 'unavailable':
      return 'Unavailable';
    case 'not-configured':
      return 'Not configured';
    case 'connected':
      if (!s.pollingOn) return 'Connected · polling paused';
      return `Connected · ${s.orgs} org${s.orgs === 1 ? '' : 's'}`;
  }
}

/** Chip tone classes are neutral/derived — never the --status-* node-state palette. */
function merakiStatusTone(s: MerakiStatus): string {
  if (s.kind === 'connected') return s.pollingOn ? 'ok' : 'paused';
  if (s.kind === 'unavailable') return 'muted';
  return 'idle';
}

export function IntegrationsCatalogPage() {
  const [meraki, setMeraki] = useState<MerakiStatus>({ kind: 'loading' });

  useEffect(() => {
    let alive = true;
    Promise.all([api.listMerakiOrgs(), api.getMerakiPolling()])
      .then(([orgs, polling]) => {
        if (!alive) return;
        setMeraki(
          orgs.length === 0
            ? { kind: 'not-configured' }
            : { kind: 'connected', orgs: orgs.length, pollingOn: polling.enabled },
        );
      })
      .catch((e: unknown) => {
        if (!alive) return;
        if (e instanceof ApiError && e.code === 'admin_unavailable') {
          setMeraki({ kind: 'unavailable' });
        } else {
          setMeraki({ kind: 'not-configured' });
        }
      });
    return () => {
      alive = false;
    };
  }, []);

  return (
    <div>
      <PageHeader
        title="Integrations"
        trail={[{ label: 'Settings' }, { label: 'Integrations' }]}
        note="Connect external systems monitored through a vendor cloud API."
      />

      <div className="integrations-grid">
        <Link className="integration-card" to="/settings/integrations/meraki">
          <div className="integration-card-head">
            <span className="integration-card-name">Cisco Meraki</span>
            <span className={`integration-chip ${merakiStatusTone(meraki)}`}>
              {merakiStatusLabel(meraki)}
            </span>
          </div>
          <p className="integration-card-desc">
            Read-only Dashboard API monitoring — availability, uplinks, and traffic across your
            Meraki organizations.
          </p>
        </Link>

        <div className="integration-card is-placeholder" aria-disabled="true">
          <div className="integration-card-head">
            <span className="integration-card-name">More integrations</span>
            <span className="integration-chip idle">Coming soon</span>
          </div>
          <p className="integration-card-desc">
            Additional vendor and notification integrations will appear here in a future release.
          </p>
        </div>
      </div>

      {meraki.kind === 'unavailable' && (
        <Card>
          <p className="muted">Integrations are unavailable in skeleton mode (no store).</p>
        </Card>
      )}
    </div>
  );
}
