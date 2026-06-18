// Troubleshoot landing — the tool catalog (handoff §1). PageHeader + an intro stat strip + the
// Analysis runs panel + the diagnostic-tool grid. Owns the launch drawer and the toast (the
// drawer is launched from any tool card here). Live job progress ticks via useRunTicker.

import { useNavigate } from 'react-router-dom';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TOOLS } from './data';
import { runningCount, useTroubleshootStore } from './store';
import { useRunTicker } from './useRunTicker';
import { AnalysisRuns } from './AnalysisRuns';
import { ToolCard } from './ToolCard';
import { LaunchDrawer } from './LaunchDrawer';
import { TroubleshootToast } from './TroubleshootToast';
import './troubleshoot.css';

export function TroubleshootCatalogPage() {
  useRunTicker();
  const navigate = useNavigate();
  const running = useTroubleshootStore((s) => runningCount(s.runs));

  return (
    <div>
      <PageHeader
        title="Troubleshoot"
        trail={[{ label: 'Troubleshoot', to: '/troubleshoot' }, { label: 'All tools' }]}
        note="Deep diagnostics for problems normal metrics and thresholds can’t see. These analyses fetch long histories and run heavier models, so they run as background jobs — start one, keep working, review the report when it lands."
      />

      <div className="ts-intro">
        <div className="ts-intro-stat">
          <span className="ts-intro-num">{TOOLS.length}</span>
          <span className="ts-intro-cap">tools</span>
        </div>
        <div className="ts-intro-sep" />
        <div className="ts-intro-stat">
          <span className="ts-intro-num">{running}</span>
          <span className="ts-intro-cap">running now</span>
        </div>
        <div className="ts-intro-sep" />
        <div className="ts-intro-stat">
          <span className="ts-intro-num">14</span>
          <span className="ts-intro-cap">runs today</span>
        </div>
        <div className="ts-intro-sep" />
        <div className="ts-intro-stat">
          <span className="ts-intro-num">2m 41s</span>
          <span className="ts-intro-cap">avg runtime</span>
        </div>
      </div>

      <Card
        title="Analysis runs"
        actions={
          <Button variant="ghost" className="btn-sm" onClick={() => navigate('/troubleshoot/runs')}>
            View all
          </Button>
        }
      >
        <AnalysisRuns />
      </Card>

      <div className="ts-section-label">
        <h2>Diagnostic tools</h2>
        <span>Pick a tool, set its scope, and run it as a job.</span>
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
