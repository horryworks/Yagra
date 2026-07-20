// SPDX-License-Identifier: AGPL-3.0-only
// Troubleshoot landing — the tool catalog (handoff §1). PageHeader + an intro stat strip (now
// computed from the real job history) + the Analysis runs panel + the diagnostic-tool grid. Owns
// the launch drawer and the toast; live job updates arrive via useTroubleshootStream.

import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TOOLS } from './data';
import { runningCount, useTroubleshootStore } from './store';
import { useTroubleshootStream } from './useTroubleshootStream';
import { AnalysisRuns } from './AnalysisRuns';
import { ToolCard } from './ToolCard';
import { LaunchDrawer } from './LaunchDrawer';
import { TroubleshootToast } from './TroubleshootToast';
import type { AnalysisJob } from '../types/api';
import './troubleshoot.css';

/** Count of jobs created in the last 24h. */
function runsToday(jobs: AnalysisJob[]): number {
  const since = Date.now() - 86_400_000;
  return jobs.filter((j) => j.created_ms >= since).length;
}

/** Mean runtime of finished jobs, formatted "Nm Ns" (or "—" if none have completed). */
function avgRuntime(jobs: AnalysisJob[]): string {
  const durs = jobs
    .filter((j) => j.started_ms != null && j.finished_ms != null)
    .map((j) => (j.finished_ms as number) - (j.started_ms as number))
    .filter((d) => d >= 0);
  if (durs.length === 0) return '—';
  const avg = durs.reduce((a, b) => a + b, 0) / durs.length / 1000;
  const m = Math.floor(avg / 60);
  const s = Math.round(avg % 60);
  return m > 0 ? `${m}m ${s}s` : `${s}s`;
}

export function TroubleshootCatalogPage() {
  const { t } = useTranslation('troubleshoot');
  useTroubleshootStream();
  const navigate = useNavigate();
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

      <Card
        title={t('nav:troubleshoot.runs')}
        actions={
          <Button
            variant="ghost"
            className="btn-sm"
            onClick={() => navigate('/troubleshoot/runs')}
          >
            {t('catalog.viewAll')}
          </Button>
        }
      >
        <AnalysisRuns />
      </Card>

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
      <TroubleshootToast />
    </div>
  );
}
