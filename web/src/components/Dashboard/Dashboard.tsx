// Dashboard: composes the panes and wires the live alert stream into the store.

import { useEffect } from 'react';
import { subscribeAlerts } from '../../services/sse';
import { useAlertStore } from '../../store';
import { AlertList } from '../AlertList/AlertList';
import { MetricChart } from '../MetricChart/MetricChart';
import { NodeDetail } from '../NodeDetail/NodeDetail';

// Placeholder demo series until the metrics range endpoint lands.
const DEMO_TS = [0, 1, 2, 3, 4, 5];
const DEMO_RTT = [8, 9, 7, 12, 10, 8];

export function Dashboard() {
  const upsertAlert = useAlertStore((s) => s.upsertAlert);

  useEffect(() => {
    // Live updates via SSE (ADR-019). EventSource only exists in the browser.
    if (typeof EventSource === 'undefined') return;
    return subscribeAlerts(upsertAlert);
  }, [upsertAlert]);

  return (
    <main className="app-main">
      <AlertList />
      <section className="pane">
        <h2>ICMP RTT</h2>
        <MetricChart title="icmp_rtt_ms" timestamps={DEMO_TS} values={DEMO_RTT} />
      </section>
      <NodeDetail nodeId="00000000-0000-0000-0000-000000000000" metric="icmp_rtt_ms" />
    </main>
  );
}
