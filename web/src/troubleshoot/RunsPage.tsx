// Analysis runs — the full async-jobs view (handoff §2, `/troubleshoot/runs`). The same run list
// the catalog summarises, on its own page. Live progress ticks via useRunTicker; cancel / retry /
// view actions are wired through the store (toasts surface their results).

import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { runningCount, useTroubleshootStore } from './store';
import { useRunTicker } from './useRunTicker';
import { AnalysisRuns } from './AnalysisRuns';
import { TroubleshootToast } from './TroubleshootToast';
import './troubleshoot.css';

export function RunsPage() {
  useRunTicker();
  const running = useTroubleshootStore((s) => runningCount(s.runs));

  return (
    <div>
      <PageHeader
        title="Analysis runs"
        trail={[{ label: 'Troubleshoot', to: '/troubleshoot' }, { label: 'Analysis runs' }]}
        note={`${running} running · background diagnostic jobs with live progress and results`}
      />
      <Card title="Runs">
        <AnalysisRuns />
      </Card>
      <TroubleshootToast />
    </div>
  );
}
