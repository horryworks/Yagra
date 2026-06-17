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
import { TextInput, Select, FieldHint } from '../components/ui/Field';
import { MetricChart } from '../components/MetricChart/MetricChart';
import { RangeControl, resolveRange, DEFAULT_RANGE, type Range } from '../components/NodeDetail/RangeControl';
import './MetricExplorerPage.css';

export function MetricExplorerPage() {
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [node, setNode] = useState('');
  const [metric, setMetric] = useState('icmp_rtt_ms');
  const [range, setRange] = useState<Range>(DEFAULT_RANGE);
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
    const { from, to } = resolveRange(range);
    api
      .getNodeMetricRange(node, metric, { from, to })
      .then((r) => setSeries(pointsToSeries(r.points)))
      .catch((e: unknown) => {
        setSeries({ timestamps: [], values: [] });
        setError(e instanceof ApiError ? e.message : 'query failed');
      })
      .finally(() => setLoading(false));
  }, [node, metric, range]);

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
              placeholder="icmp_rtt_ms"
            />
            <FieldHint>TSDB series name (e.g. icmp_rtt_ms, snmp_oid_*) — not a display label.</FieldHint>
          </label>
          <div className="form-label">
            Range
            <RangeControl value={range} onChange={setRange} />
          </div>
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
