// Metric explorer (Metrics ▸ Metric explorer). Pick a node + metric + time window and chart
// the range from the TSDB. Metric name is free-form (validated server-side) since the metric
// catalog API doesn't exist yet; icmp_rtt_ms is the one always present today.

import { useCallback, useEffect, useState } from 'react';
import { pointsToSeries } from '../lib/format';
import { api, ApiError } from '../services/api';
import type { NodeSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput, Select } from '../components/ui/Field';
import { MetricChart } from '../components/MetricChart/MetricChart';
import './MetricExplorerPage.css';

const RANGES: { label: string; secs: number }[] = [
  { label: 'Last 1h', secs: 3600 },
  { label: 'Last 6h', secs: 6 * 3600 },
  { label: 'Last 24h', secs: 24 * 3600 },
];

export function MetricExplorerPage() {
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [node, setNode] = useState('');
  const [metric, setMetric] = useState('icmp_rtt_ms');
  const [rangeSecs, setRangeSecs] = useState(RANGES[0].secs);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .listNodes()
      .then((ns) => {
        setNodes(ns);
        setNode((cur) => cur || ns[0]?.id || '');
        // Nothing to query against — stop waiting so the chart shows its empty state.
        if (ns.length === 0) setLoading(false);
      })
      .catch(() => setLoading(false));
  }, []);

  const run = useCallback(() => {
    if (!node || !metric) return;
    setError(null);
    const to = Math.floor(Date.now() / 1000);
    api
      .getNodeMetricRange(node, metric, { from: to - rangeSecs, to })
      .then((r) => setSeries(pointsToSeries(r.points)))
      .catch((e: unknown) => {
        setSeries({ timestamps: [], values: [] });
        setError(e instanceof ApiError ? e.message : 'query failed');
      })
      .finally(() => setLoading(false));
  }, [node, metric, rangeSecs]);

  useEffect(() => {
    run();
  }, [run]);

  return (
    <div>
      <PageHeader title="Metric explorer" trail={[{ label: 'Metrics' }, { label: 'Metric explorer' }]} />

      <Card title="Query">
        <div className="mx-form form-row">
          <label className="form-label">
            Node
            <Select value={node} onChange={(e) => setNode(e.target.value)}>
              {nodes.length === 0 && <option value="">No nodes</option>}
              {nodes.map((n) => (
                <option key={n.id} value={n.id}>
                  {n.name}
                </option>
              ))}
            </Select>
          </label>
          <label className="form-label">
            Metric
            <TextInput
              className="mono"
              value={metric}
              onChange={(e) => setMetric(e.target.value)}
            />
          </label>
          <label className="form-label">
            Range
            <Select value={rangeSecs} onChange={(e) => setRangeSecs(Number(e.target.value))}>
              {RANGES.map((r) => (
                <option key={r.secs} value={r.secs}>
                  {r.label}
                </option>
              ))}
            </Select>
          </label>
          <Button variant="primary" onClick={run} disabled={!node || !metric}>
            Run
          </Button>
        </div>
      </Card>

      <Card title={metric} className="mx-chart-card">
        {error && <p className="form-error">{error}</p>}
        {!error && series.timestamps.length === 0 ? (
          <p className="muted">{loading ? 'Loading…' : 'No data for this query.'}</p>
        ) : (
          series.timestamps.length > 0 && (
            <MetricChart title={metric} timestamps={series.timestamps} values={series.values} />
          )
        )}
      </Card>
    </div>
  );
}
