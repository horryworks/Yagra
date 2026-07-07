// About (Settings ▸ About). Product identity + versions in one place. The running core/API
// version comes from the backend (`GET /api/v1/version`, public); the WebUI build version is
// baked in at compile time (Vite `define`, from package.json). Showing both makes a core/web
// skew during a rolling upgrade visible. Read-only, no auth needed to render.

import { useEffect, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { api } from '../services/api';
import './AboutPage.css';

const REPOSITORY = 'https://github.com/horryworks/Yagra';

/** One label/value row. `mono` renders the value in the monospace family (for versions). */
function InfoRow({ label, children, mono }: { label: string; children: ReactNode; mono?: boolean }) {
  return (
    <div className="about-row">
      <div className="about-label muted">{label}</div>
      <div className={mono ? 'about-value mono' : 'about-value'}>{children}</div>
    </div>
  );
}

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
    <div>
      <PageHeader
        title={t('about.title')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('about.title') }]}
        note={t('about.note')}
      />
      <Card title={t('nav:shell.wordmark')}>
        <p className="about-tagline muted">{t('about.tagline')}</p>
        <div className="about-grid">
          <InfoRow label={t('about.coreVersion')} mono>
            {core}
          </InfoRow>
          <InfoRow label={t('about.webuiVersion')} mono>
            {__APP_VERSION__}
          </InfoRow>
          <InfoRow label={t('about.repository')}>
            <a href={REPOSITORY} target="_blank" rel="noreferrer">
              {REPOSITORY}
            </a>
          </InfoRow>
          <InfoRow label={t('about.license')}>MIT</InfoRow>
        </div>
      </Card>
    </div>
  );
}
