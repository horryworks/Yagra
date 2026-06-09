// Dashboard: composes the panes, loads the node inventory, charts the selected node's
// RTT history from the range endpoint (polled for a live feel), and wires the alert
// stream into the store.

import { useEffect, useState } from 'react';
import { pointsToSeries } from '../../lib/format';
import { api, ApiError } from '../../services/api';
import { subscribeAlerts } from '../../services/sse';
import { useAlertStore } from '../../store';
import type { NodeSummary } from '../../types/api';
import { AlertList } from '../AlertList/AlertList';
import { MetricChart } from '../MetricChart/MetricChart';
import { NodeDetail } from '../NodeDetail/NodeDetail';

const METRIC = 'icmp_rtt_ms';
const REFRESH_MS = 15_000;

export function Dashboard() {
  const upsertAlert = useAlertStore((s) => s.upsertAlert);
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [chartError, setChartError] = useState<string | null>(null);

  // Live alert updates via SSE (ADR-019). EventSource only exists in the browser.
  useEffect(() => {
    if (typeof EventSource === 'undefined') return;
    return subscribeAlerts(upsertAlert);
  }, [upsertAlert]);

  // Load the inventory once; default the selection to the first node.
  useEffect(() => {
    let cancelled = false;
    api
      .listNodes()
      .then((ns) => {
        if (cancelled) return;
        setNodes(ns);
        setSelected((cur) => cur ?? ns[0]?.id ?? null);
      })
      .catch(() => {
        /* leave inventory empty; the chart shows a no-data hint */
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Poll the RTT range for the selected node (incremental live refresh).
  useEffect(() => {
    if (!selected) return;
    let cancelled = false;
    const load = () => {
      api
        .getNodeMetricRange(selected, METRIC)
        .then((r) => {
          if (cancelled) return;
          setSeries(pointsToSeries(r.points));
          setChartError(null);
        })
        .catch((e: unknown) => {
          if (!cancelled) setChartError(e instanceof ApiError ? e.message : 'request failed');
        });
    };
    load();
    const id = setInterval(load, REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [selected]);

  return (
    <main className="app-main">
      <AlertList />
      <section className="pane">
        <h2>ICMP RTT</h2>
        {nodes.length > 1 && (
          <div className="node-tabs">
            {nodes.map((n) => (
              <button
                key={n.id}
                className={n.id === selected ? 'node-tab active' : 'node-tab'}
                onClick={() => setSelected(n.id)}
              >
                {n.name}
              </button>
            ))}
          </div>
        )}
        {chartError && <p className="muted">{chartError}</p>}
        {!chartError && series.timestamps.length === 0 && <p className="muted">No data yet…</p>}
        {series.timestamps.length > 0 && (
          <MetricChart title={METRIC} timestamps={series.timestamps} values={series.values} />
        )}
      </section>
      {selected && <NodeDetail nodeId={selected} metric={METRIC} />}
    </main>
  );
}
