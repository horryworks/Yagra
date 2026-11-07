// SPDX-License-Identifier: AGPL-3.0-only
// Analysis runs — the full async-jobs view (handoff §2, `/troubleshoot/runs`). The same run list
// the catalog summarises, on its own page. Live progress arrives over the SSE stream AppShell mounts; cancel / retry /
// view actions are wired through the store (toasts surface their results).

import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Page } from '../components/ui/Page';
import { AnalysisRuns } from './AnalysisRuns';
import './troubleshoot.css';

export function RunsPage() {
  const { t } = useTranslation('troubleshoot');
  return (
    <Page layout="list">
      <PageHeader
        title={t('nav:troubleshoot.runs')}
        trail={[{ label: t('nav:sections.troubleshoot'), to: '/troubleshoot' }, { label: t('nav:troubleshoot.runs') }]}
      />
      <AnalysisRuns />
    </Page>
  );
}
