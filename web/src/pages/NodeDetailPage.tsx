// Node detail. Reached by drilling from All nodes (breadcrumb trail). Page-internal sub-tabs
// (§4); only Overview has data today — latest RTT reading + RTT range chart from the metric
// endpoints. Interfaces needs a per-interface API that doesn't exist yet, so it's a marked
// placeholder rather than a fake table.

import { useEffect, useState } from 'react';
import { useParams } from 'react-router-dom';
import { formatRtt, pointsToSeries } from '../lib/format';
import { api, ApiError } from '../services/api';
import type { MetricReading } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Tabs } from '../components/ui/Tabs';
import { MetricChart } from '../components/MetricChart/MetricChart';
import './NodeDetailPage.css';

const METRIC = 'icmp_rtt_ms';

export function NodeDetailPage() {
  const { nodeId = '' } = useParams();
  const [tab, setTab] = useState('overview');
  const [reading, setReading] = useState<MetricReading | null>(null);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setReading(null);
    setError(null);
    api
      .getNodeMetric(nodeId, METRIC)
      .then((r) => !cancelled && setReading(r))
      .catch((e: unknown) =>
        !cancelled && setError(e instanceof ApiError ? e.message : 'no reading'),
      );
    api
      .getNodeMetricRange(nodeId, METRIC)
      .then((r) => !cancelled && setSeries(pointsToSeries(r.points)))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [nodeId]);

  return (
    <div>
      <PageHeader
        title={<span className="mono">{nodeId}</span>}
        trail={[
          { label: 'Nodes' },
          { label: 'All nodes', to: '/nodes' },
          { label: nodeId },
        ]}
      />

      <Tabs
        tabs={[
          { key: 'overview', label: 'Overview' },
          { key: 'interfaces', label: 'Interfaces' },
        ]}
        active={tab}
        onChange={setTab}
      />

      <div className="nodedetail-body">
        {tab === 'overview' && (
          <div className="nodedetail-grid">
            <Card title="Latest reading">
              {error && <p className="muted">{error}</p>}
              {!error && !reading && <p className="muted">Loading…</p>}
              {reading && (
                <div className="nodedetail-reading">
                  <div className="nodedetail-metric">{reading.metric}</div>
                  <div className="nodedetail-value">
                    {reading.metric === METRIC ? formatRtt(reading.value) : reading.value}
                  </div>
                </div>
              )}
            </Card>
            <Card title="RTT history">
              {series.timestamps.length === 0 ? (
                <p className="muted">No history yet…</p>
              ) : (
                <MetricChart title={METRIC} timestamps={series.timestamps} values={series.values} />
              )}
            </Card>
          </div>
        )}

        {tab === 'interfaces' && (
          <Card>
            <p className="muted">
              Per-interface metrics need an interfaces API (SNMP collection), which isn't wired
              yet. This tab will list interfaces with utilization once that lands.
            </p>
          </Card>
        )}
      </div>
    </div>
  );
}
