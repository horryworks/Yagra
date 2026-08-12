// SPDX-License-Identifier: AGPL-3.0-only
// Troubleshoot landing — the tool catalog (handoff §1). PageHeader + an intro stat strip (now
// computed from the real job history) + the diagnostic-tool grid. Owns the launch drawer; live job
// updates and the completion toast come from AppShell. The run list itself lives only on
// /troubleshoot/runs — this page used to render the same <AnalysisRuns/> unfiltered, which was a
// verbatim duplicate of that page pushing the tool grid down.

import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { TOOLS } from './data';
import { runningCount, useTroubleshootStore } from './store';
import { ToolCard } from './ToolCard';
import { LaunchDrawer } from './LaunchDrawer';
import { avgRuntime, runsToday } from './catalogStats';
import './troubleshoot.css';

export function TroubleshootCatalogPage() {
  const { t } = useTranslation('troubleshoot');
  const jobs = useTroubleshootStore((s) => s.jobs);
  const running = runningCount(jobs);

  return (
    <div>
      <PageHeader
        title={t('nav:sections.troubleshoot')}
        trail={[{ label: t('nav:sections.troubleshoot'), to: '/troubleshoot' }, { label: t('nav:troubleshoot.all') }]}
        note={t('catalog.note')}
      />

      <div className="ts-intro">
        <div className="ts-intro-stat">
          <span className="ts-intro-num">{TOOLS.length}</span>
          <span className="ts-intro-cap">{t('catalog.stat.tools')}</span>
        </div>
        <div className="ts-intro-sep" />
        <div className="ts-intro-stat">
          <span className="ts-intro-num">{running}</span>
          <span className="ts-intro-cap">{t('catalog.stat.runningNow')}</span>
        </div>
        <div className="ts-intro-sep" />
        <div className="ts-intro-stat">
          <span className="ts-intro-num">{runsToday(jobs)}</span>
          <span className="ts-intro-cap">{t('catalog.stat.runsToday')}</span>
        </div>
        <div className="ts-intro-sep" />
        <div className="ts-intro-stat">
          <span className="ts-intro-num">{avgRuntime(jobs)}</span>
          <span className="ts-intro-cap">{t('catalog.stat.avgRuntime')}</span>
        </div>
      </div>

      <div className="ts-section-label">
        <h2>{t('catalog.toolsHeading')}</h2>
        <span>{t('catalog.toolsSub')}</span>
      </div>
      <div className="ts-tool-grid">
        {TOOLS.map((t) => (
          <ToolCard key={t.id} tool={t} />
        ))}
      </div>

      <LaunchDrawer />
    </div>
  );
}
