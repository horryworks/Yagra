// Dashboard / Overview (NOC). The §8 board, scoped to widgets that have real backing data
// today: status summary (node states), active alerts (live SSE), and a Top-N-style RTT chart
// for a chosen node. Other recommended widgets (heatmap, SLA, throughput) wait on endpoints.

import { useCallback, useEffect, useState } from 'react';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { formatRtt, pointsToSeries } from '../lib/format';
import { api, ApiError } from '../services/api';
import { useAlertStream } from '../hooks/useAlertStream';
import type { NodeSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Select } from '../components/ui/Field';
import { MetricChart } from '../components/MetricChart/MetricChart';
import { StatusSummary } from '../widgets/StatusSummary';
import { AlertRows } from '../widgets/AlertRows';
import './DashboardPage.css';

const METRIC = 'icmp_rtt_ms';
const REFRESH_MS = 15_000;

export function DashboardPage() {
  useAlertStream();
  const navigate = useNavigate();
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [nodesLoading, setNodesLoading] = useState(true);
  // The charted node lives in the URL (`?node=<id>`) so a reload restores the same chart instead of
  // snapping back to the first node (design-guidelines.md "画面状態の永続化").
  const [searchParams, setSearchParams] = useSearchParams();
  const selected = searchParams.get('node');
  const setSelected = useCallback(
    (id: string) => {
      const params = new URLSearchParams(searchParams);
      if (id) params.set('node', id);
      else params.delete('node');
      setSearchParams(params, { replace: true });
    },
    [searchParams, setSearchParams],
  );
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [chartError, setChartError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .listNodes()
      .then((ns) => {
        if (cancelled) return;
        setNodes(ns);
      })
      .catch(() => undefined)
      .finally(() => {
        if (!cancelled) setNodesLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Default/validate the charted node once the list loads: keep the URL's pin if it still exists,
  // otherwise fall back to the first node (written back to the URL so a reload is stable).
  useEffect(() => {
    if (nodes.length === 0) return;
    const cur = searchParams.get('node');
    if (cur && nodes.some((n) => n.id === cur)) return;
    const params = new URLSearchParams(searchParams);
    params.set('node', nodes[0].id);
    setSearchParams(params, { replace: true });
  }, [nodes, searchParams, setSearchParams]);

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
    <div>
      <PageHeader title="Overview" trail={[{ label: 'Dashboard' }, { label: 'Overview' }]} />

      <div className="dash-grid">
        <Card title="Status summary" className="dash-span2">
          <StatusSummary nodes={nodes} loading={nodesLoading} />
        </Card>

        <Card
          title="ICMP RTT"
          actions={
            nodes.length > 1 ? (
              <Select value={selected ?? ''} onChange={(e) => setSelected(e.target.value)}>
                {nodes.map((n) => (
                  <option key={n.id} value={n.id}>
                    {n.name}
                  </option>
                ))}
              </Select>
            ) : undefined
          }
        >
          {chartError && <p className="muted">{chartError}</p>}
          {!chartError && series.timestamps.length === 0 && <p className="muted">No data yet…</p>}
          {series.timestamps.length > 0 && (
            <MetricChart
              title="ICMP RTT"
              timestamps={series.timestamps}
              values={series.values}
              yFormat={(v) => `${Math.round(v)} ms`}
              legendFormat={formatRtt}
            />
          )}
        </Card>

        <Card
          title="Active alerts"
          className="dash-span2"
          actions={
            <Button variant="ghost" onClick={() => navigate('/alerts')}>
              View all
            </Button>
          }
        >
          <AlertRows limit={8} empty="No active alerts. All monitored nodes are healthy." />
        </Card>
      </div>
    </div>
  );
}
