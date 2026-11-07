// SPDX-License-Identifier: AGPL-3.0-only
// About (Settings ▸ About). Product identity + versions in one place. The running core/API
// version comes from the backend (`GET /api/v1/version`, public); the WebUI build version is
// baked in at compile time (Vite `define`, from package.json). Showing both makes a core/web
// skew during a rolling upgrade visible. Read-only, no auth needed to render.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Page } from '../components/ui/Page';
import { Card } from '../components/ui/Card';
import { Facts } from '../components/ui/Facts';
import { api } from '../services/api';
import './AboutPage.css';

const REPOSITORY = 'https://github.com/horryworks/Yagra';

export function AboutPage() {
  const { t } = useTranslation('settings');
  const [coreVersion, setCoreVersion] = useState<string | null>(null);
  const [coreError, setCoreError] = useState(false);

  useEffect(() => {
    let cancelled = false;
    api
      .getVersion()
      .then((v) => {
        if (!cancelled) setCoreVersion(v.core);
      })
      .catch(() => {
        if (!cancelled) setCoreError(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const core = coreError ? t('about.unavailable') : (coreVersion ?? '…');

  return (
    <Page layout="sections">
      <PageHeader
        title={t('about.title')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('about.title') }]}
      />
      <Card title={t('nav:shell.wordmark')}>
        <p className="about-tagline muted">{t('about.tagline')}</p>
        <Facts
          items={[
            { label: t('about.coreVersion'), value: <span className="mono">{core}</span> },
            {
              label: t('about.webuiVersion'),
              value: <span className="mono">{__APP_VERSION__}</span>,
            },
            {
              label: t('about.repository'),
              value: (
                <a href={REPOSITORY} target="_blank" rel="noreferrer">
                  {REPOSITORY}
                </a>
              ),
            },
            {
              label: t('about.license'),
              value: (
                <a href={`${REPOSITORY}/blob/main/LICENSE`} target="_blank" rel="noreferrer">
                  AGPL-3.0-only
                </a>
              ),
            },
          ]}
        />
      </Card>
    </Page>
  );
}
