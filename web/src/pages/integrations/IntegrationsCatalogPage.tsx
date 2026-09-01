// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Integrations. Catalog of external systems Yagra can monitor through a vendor cloud
// API. Today the only integration is Cisco Meraki; the page is a card grid so more vendors slot in
// without restructuring. Each connected integration links to its own detail page (the Meraki page
// moved to /settings/integrations/meraki). The status chip is derived, not authored: it reflects
// the live org count + polling switch so the operator sees "Connected · 2 orgs" at a glance.

import { useEffect, useState } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import { PageHeader } from '../../components/ui/PageHeader';
import {
  merakiStatusLabel,
  merakiStatusTone,
  type MerakiStatus,
} from './merakiStatus';
import { classifyLoadError } from '../../lib/loadState';
import { LoadBlockNotice } from '../../components/ui/LoadBlockNotice';
import './IntegrationsCatalogPage.css';

export function IntegrationsCatalogPage() {
  const { t } = useTranslation('system');
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
        const block = classifyLoadError(e);
        setMeraki(block ? { kind: block } : { kind: 'not-configured' });
      });
    return () => {
      alive = false;
    };
  }, []);

  return (
    <div>
      <PageHeader
        title={t('nav:settings.integrations')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.integrations') }]}
        note={t('integrations.note')}
      />

      <div className="integrations-grid">
        <Link className="integration-card" to="/settings/integrations/meraki">
          <div className="integration-card-head">
            <span className="integration-card-name">{t('meraki.name')}</span>
            <span className={`integration-chip ${merakiStatusTone(meraki)}`}>
              {merakiStatusLabel(meraki, t)}
            </span>
          </div>
          <p className="integration-card-desc">{t('integrations.meraki.desc')}</p>
        </Link>

        <div className="integration-card is-placeholder" aria-disabled="true">
          <div className="integration-card-head">
            <span className="integration-card-name">{t('integrations.more.name')}</span>
            <span className="integration-chip idle">{t('nav:shell.comingSoonBadge')}</span>
          </div>
          <p className="integration-card-desc">{t('integrations.more.desc')}</p>
        </div>
      </div>

      {(meraki.kind === 'unavailable' || meraki.kind === 'forbidden') && (
        <LoadBlockNotice block={meraki.kind} unavailable={t('integrations.unavailable')} />
      )}
    </div>
  );
}
