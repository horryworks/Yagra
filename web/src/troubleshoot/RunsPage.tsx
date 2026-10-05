// SPDX-License-Identifier: AGPL-3.0-only
// Analysis runs — the full async-jobs view (handoff §2, `/troubleshoot/runs`). The same run list
// the catalog summarises, on its own page. Live progress arrives over the SSE stream AppShell mounts; cancel / retry /
// view actions are wired through the store (toasts surface their results).

import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { AnalysisRuns } from './AnalysisRuns';
import './troubleshoot.css';

export function RunsPage() {
  const { t } = useTranslation('troubleshoot');
  return (
    <div>
      <PageHeader
        title={t('nav:troubleshoot.runs')}
        trail={[{ label: t('nav:sections.troubleshoot'), to: '/troubleshoot' }, { label: t('nav:troubleshoot.runs') }]}
      />
      <Card title={t('runs.card')}>
        <AnalysisRuns filterable />
      </Card>
    </div>
  );
}
