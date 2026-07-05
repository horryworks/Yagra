// About (Settings ▸ About). Product identity + versions in one place. The running core/API
// version comes from the backend (`GET /api/v1/version`, public); the WebUI build version is
// baked in at compile time (Vite `define`, from package.json). Showing both makes a core/web
// skew during a rolling upgrade visible. Read-only, no auth needed to render.

import { useEffect, useState, type ReactNode } from 'react';
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

  const core = coreError ? 'unavailable' : (coreVersion ?? '…');

  return (
    <div>
      <PageHeader
        title="About"
        trail={[{ label: 'Settings' }, { label: 'About' }]}
        note="Product identity and running versions."
      />
      <Card title="Yagra">
        <p className="about-tagline muted">
          Network monitoring system — ICMP / SNMP / URL monitoring, discovery, alerting and
          dashboards, built to scale to tens of thousands of nodes.
        </p>
        <div className="about-grid">
          <InfoRow label="Core / API version" mono>
            {core}
          </InfoRow>
          <InfoRow label="WebUI version" mono>
            {__APP_VERSION__}
          </InfoRow>
          <InfoRow label="Repository">
            <a href={REPOSITORY} target="_blank" rel="noreferrer">
              {REPOSITORY}
            </a>
          </InfoRow>
          <InfoRow label="License">MIT</InfoRow>
        </div>
      </Card>
    </div>
  );
}
